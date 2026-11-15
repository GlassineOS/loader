//! Log lines to COM1 by port I/O. Never through ConOut: on a PC ConOut is drawn on the
//! screen by the firmware's graphics console, which would break 011 FR-016.
use core::arch::asm;
use core::fmt;

const COM1: u16 = 0x3f8;

fn outb(port: u16, v: u8) {
    unsafe { asm!("out dx, al", in("dx") port, in("al") v, options(nomem, nostack, preserves_flags)) }
}
fn inb(port: u16) -> u8 {
    let v: u8;
    unsafe { asm!("in al, dx", out("al") v, in("dx") port, options(nomem, nostack, preserves_flags)) }
    v
}

pub struct Serial;

impl fmt::Write for Serial {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for b in s.bytes() {
            if b == b'\n' {
                put(b'\r');
            }
            put(b);
        }
        Ok(())
    }
}

fn put(b: u8) {
    let mut spins = 0u32;
    while inb(COM1 + 5) & 0x20 == 0 && spins < 100_000 {
        spins += 1;
    }
    outb(COM1, b);
}

#[macro_export]
macro_rules! log {
    ($($t:tt)*) => {{
        use core::fmt::Write as _;
        let _ = write!($crate::serial::Serial, "[loader {:>6}] ", $crate::time::now_ms());
        let _ = writeln!($crate::serial::Serial, $($t)*);
    }};
}
