// SPDX-License-Identifier: GPL-2.0-or-later
//! The displays the loader draws on. The rest of the loader sees only [`Display`]; the
//! firmware's graphics output (GOP) is one backend, and another kind of output can be
//! added next to it.
use alloc::boxed::Box;
use alloc::vec::Vec;

pub mod epdfw;
mod gop;

/// One display, in its current mode. Pixels are `0x00RRGGBB`, row by row.
pub trait Display {
    fn size(&self) -> (u32, u32);
    /// Draws the `w`×`h` rectangle at (`x`, `y`) from `px`, whose rows are `stride`
    /// pixels apart.
    fn draw(&mut self, x: u32, y: u32, w: u32, h: u32, px: &[u32], stride: usize);
    /// Reads the whole screen into `buf` (`size().0 * size().1` pixels).
    fn read(&mut self, buf: &mut [u32]) -> bool;
    /// What it is, for the log.
    fn describe(&self) -> alloc::string::String;
}

/// Every display the loader owns, the first one first.
pub fn all() -> Vec<Box<dyn Display>> {
    let mut out: Vec<Box<dyn Display>> = Vec::new();
    for d in gop::all() {
        out.push(Box::new(d));
    }
    out
}
