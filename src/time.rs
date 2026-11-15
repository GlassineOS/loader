//! Milliseconds from the TSC, calibrated once against the firmware's Stall().
use core::sync::atomic::{AtomicU64, Ordering};
use core::time::Duration;

static T0: AtomicU64 = AtomicU64::new(0);
static PER_MS: AtomicU64 = AtomicU64::new(0);

fn rdtsc() -> u64 {
    unsafe { core::arch::x86_64::_rdtsc() }
}

pub fn calibrate() {
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
    (rdtsc() - T0.load(Ordering::Relaxed)) / per
}

pub extern "C" fn lv_tick() -> u32 {
    now_ms() as u32
}
