// SPDX-License-Identifier: GPL-2.0-or-later
//! Log lines to the serial line, never to the firmware's text console: on a PC that
//! console is drawn on the display, which would break 011 FR-016.
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;
use core::sync::atomic::{AtomicBool, Ordering};
use uefi::boot::{self, OpenProtocolAttributes, OpenProtocolParams, ScopedProtocol};
use uefi::proto::console::serial::Serial;

pub struct Line;

/// Whether log lines reach the serial line. Off in hand-back mode unless the parent asks
/// for it (`glassine.log=serial`), so that no serial protocol is called on a firmware
/// whose serial line nobody reads.
static SERIAL_ON: AtomicBool = AtomicBool::new(true);
/// Whether log lines are also kept in memory (hand-back mode: for the probe report).
static KEEP: AtomicBool = AtomicBool::new(false);
static mut KEPT: Vec<u8> = Vec::new();
/// The most kept: the probe report has room for about this much log.
const KEEP_MAX: usize = 24 * 1024;

pub fn set_serial(on: bool) {
    SERIAL_ON.store(on, Ordering::Relaxed);
}

pub fn keep() {
    KEEP.store(true, Ordering::Relaxed);
}

/// The kept log lines.
#[allow(static_mut_refs)]
pub fn kept() -> Vec<String> {
    // The loader runs on one processor with no interrupts of its own.
    let b = unsafe { &KEPT };
    String::from_utf8_lossy(b).lines().map(String::from).collect()
}

impl fmt::Write for Line {
    #[allow(static_mut_refs)]
    fn write_str(&mut self, s: &str) -> fmt::Result {
        if KEEP.load(Ordering::Relaxed) {
            let k = unsafe { &mut KEPT };
            if k.len() + s.len() <= KEEP_MAX {
                k.extend_from_slice(s.as_bytes());
            }
        }
        if !SERIAL_ON.load(Ordering::Relaxed) {
            return Ok(());
        }
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
