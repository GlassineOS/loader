// SPDX-License-Identifier: GPL-2.0-or-later
//! The GlassineOS loader's decisions, formats and writes, as GlassineOS/boot's
//! `docs/boot-partition.md` (format 1) and specs 010 and 011 give them. Nothing here
//! calls the firmware: the loader binary provides a [`plan::Platform`], and the host
//! tests provide a fake one.
#![no_std]

extern crate alloc;
#[cfg(test)]
extern crate std;

pub mod deploys;
pub mod gspl;
pub mod plan;
pub mod record;
pub mod setrec;

use alloc::string::String;
use sha2::{Digest, Sha256};

/// Lower-case hex of `b`.
pub fn hex(b: &[u8]) -> String {
    const D: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(b.len() * 2);
    for x in b {
        s.push(D[(x >> 4) as usize] as char);
        s.push(D[(x & 15) as usize] as char);
    }
    s
}

/// The sha256 of `b`, as 64 lower-case hex digits.
pub fn sha256_hex(b: &[u8]) -> String {
    hex(&Sha256::digest(b))
}
