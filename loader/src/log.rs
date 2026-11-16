// SPDX-License-Identifier: GPL-2.0-or-later
//! Log lines to the serial line, never to the firmware's text console: on a PC that
//! console is drawn on the display, which would break 011 FR-016.
use core::fmt;
use uefi::boot::{self, OpenProtocolAttributes, OpenProtocolParams, ScopedProtocol};
use uefi::proto::console::serial::Serial;

pub struct Line;

impl fmt::Write for Line {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for part in s.split_inclusive('\n') {
            match part.strip_suffix('\n') {
                Some(t) => {
                    crate::arch::serial_write(t.as_bytes());
                    crate::arch::serial_write(b"\r\n");
                }
                None => crate::arch::serial_write(part.as_bytes()),
            }
        }
        Ok(())
    }
}

#[macro_export]
macro_rules! log {
    ($($t:tt)*) => {{
        use core::fmt::Write as _;
        let _ = write!($crate::log::Line, "[loader {:>6}] ", $crate::arch::now_ms());
        let _ = writeln!($crate::log::Line, $($t)*);
    }};
}

pub fn line(args: fmt::Arguments) {
    log!("{}", args);
}

static mut SERIAL: Option<ScopedProtocol<Serial>> = None;
static mut SERIAL_TRIED: bool = false;

/// Writes through the first serial I/O protocol the firmware offers, shared with the
/// firmware's own use of it. Used where the processor has no fixed serial port.
#[allow(dead_code, static_mut_refs)]
pub fn firmware_serial_write(bytes: &[u8]) {
    // The loader runs on one processor with no interrupts of its own.
    unsafe {
        if !SERIAL_TRIED {
            SERIAL_TRIED = true;
            if let Ok(h) = boot::get_handle_for_protocol::<Serial>() {
                SERIAL = boot::open_protocol::<Serial>(
                    OpenProtocolParams { handle: h, agent: boot::image_handle(), controller: None },
                    OpenProtocolAttributes::GetProtocol,
                )
                .ok();
            }
        }
        if let Some(s) = SERIAL.as_mut() {
            let _ = s.write(bytes);
        }
    }
}
