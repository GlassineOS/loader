// SPDX-License-Identifier: GPL-2.0-or-later
//! x86-64: milliseconds from the TSC, calibrated once against the firmware's Stall();
//! log lines to COM1 by port I/O (a PC without one ignores the writes).
use core::arch::asm;
use core::sync::atomic::{AtomicU64, Ordering};
use core::time::Duration;

pub const NAME: &str = "x86-64";

static T0: AtomicU64 = AtomicU64::new(0);
static PER_MS: AtomicU64 = AtomicU64::new(0);

fn rdtsc() -> u64 {
    unsafe { core::arch::x86_64::_rdtsc() }
}

pub fn init_clock() {
    let a = rdtsc();
    uefi::boot::stall(Duration::from_millis(20));
    let b = rdtsc();
    PER_MS.store(((b - a) / 20).max(1), Ordering::Relaxed);
    T0.store(a, Ordering::Relaxed);
}

pub fn now_ms() -> u64 {
    let per = PER_MS.load(Ordering::Relaxed);
    if per == 0 {
        return 0;
    }
    rdtsc().wrapping_sub(T0.load(Ordering::Relaxed)) / per
}

const COM1: u16 = 0x3f8;

fn outb(port: u16, v: u8) {
    unsafe { asm!("out dx, al", in("dx") port, in("al") v, options(nomem, nostack, preserves_flags)) }
}
fn inb(port: u16) -> u8 {
    let v: u8;
    unsafe { asm!("in al, dx", out("al") v, in("dx") port, options(nomem, nostack, preserves_flags)) }
    v
}

pub fn serial_write(bytes: &[u8]) {
    for &b in bytes {
        let mut spins = 0u32;
        while inb(COM1 + 5) & 0x20 == 0 && spins < 100_000 {
            spins += 1;
        }
        outb(COM1, b);
    }
}

pub fn halt() -> ! {
    loop {
        unsafe { asm!("hlt") };
    }
}
