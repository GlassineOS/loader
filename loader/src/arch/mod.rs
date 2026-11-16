// SPDX-License-Identifier: GPL-2.0-or-later
//! What depends on the processor: the clock and the serial log line. Everything else in
//! the loader is the same on every processor it is built for.
#[cfg(target_arch = "aarch64")]
mod aarch64;
#[cfg(target_arch = "aarch64")]
pub use aarch64::*;
#[cfg(target_arch = "x86_64")]
mod x86_64;
#[cfg(target_arch = "x86_64")]
pub use x86_64::*;
