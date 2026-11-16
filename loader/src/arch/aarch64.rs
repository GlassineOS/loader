// SPDX-License-Identifier: GPL-2.0-or-later
//! arm64: milliseconds from the generic timer (CNTVCT_EL0 at CNTFRQ_EL0); log lines
//! through the firmware's serial I/O protocol, since arm64 has no fixed serial port.
use core::arch::asm;
use core::sync::atomic::{AtomicU64, Ordering};

pub const NAME: &str = "arm64";

static T0: AtomicU64 = AtomicU64::new(0);
static PER_MS: AtomicU64 = AtomicU64::new(0);

fn cntvct() -> u64 {
    let v: u64;
    unsafe { asm!("isb", "mrs {}, cntvct_el0", out(reg) v, options(nomem, nostack)) };
    v
}

pub fn init_clock() {
    let f: u64;
    unsafe { asm!("mrs {}, cntfrq_el0", out(reg) f, options(nomem, nostack)) };
    PER_MS.store((f / 1000).max(1), Ordering::Relaxed);
    T0.store(cntvct(), Ordering::Relaxed);
}

pub fn now_ms() -> u64 {
    let per = PER_MS.load(Ordering::Relaxed);
    if per == 0 {
        return 0;
    }
    cntvct().wrapping_sub(T0.load(Ordering::Relaxed)) / per
}

pub fn serial_write(bytes: &[u8]) {
    crate::log::firmware_serial_write(bytes);
}

pub fn halt() -> ! {
    loop {
        unsafe { asm!("wfi") };
    }
}
