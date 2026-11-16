// SPDX-License-Identifier: GPL-2.0-or-later
//! The hand-back partition (GlassineOS/boot `docs/handback.md`, format 1): the loader
//! record's two slots, raw at fixed offsets, the probe area and the loader image header.
//! What reaches the disk goes through [`Blocks`], which the loader binary provides with
//! the firmware's block I/O and the host tests with a fake that can cut a write.
use alloc::vec;
use alloc::vec::Vec;

use crate::gspl::crc32;
use crate::plan::{ReadErr, RecordStore};
use crate::record::SLOT_BYTES;

/// Where each slot starts, a then b.
pub const SLOT_OFFSETS: [u64; 2] = [0, 16384];
pub const PROBE_OFFSET: u64 = 32768;
pub const PROBE_BYTES: usize = 65536;
pub const LOADER_HEADER_OFFSET: u64 = 1 << 20;
pub const LOADER_IMAGE_OFFSET: u64 = LOADER_HEADER_OFFSET + 4096;
pub const LOADER_MAX: u64 = (3 << 20) - 4096;
/// The smallest hand-back partition.
pub const MIN_PARTITION: u64 = 4 << 20;

/// A block device: whole blocks at a block address, as `EFI_BLOCK_IO_PROTOCOL` gives
/// them.
pub trait Blocks {
    fn block_size(&self) -> u32;
    /// The number of blocks.
    fn blocks(&self) -> u64;
    fn read_only(&self) -> bool;
    fn read(&mut self, lba: u64, buf: &mut [u8]) -> bool;
    fn write(&mut self, lba: u64, buf: &[u8]) -> bool;
    fn flush(&mut self) -> bool;
}

/// Why the partition can't be used at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unusable {
    /// A block size that is zero or doesn't divide 4096.
    BlockSize(u32),
    /// Smaller than [`MIN_PARTITION`].
    TooSmall(u64),
}

/// The loader record and the probe area on a hand-back partition.
pub struct Raw<B: Blocks> {
    pub dev: B,
    bs: u64,
}

impl<B: Blocks> Raw<B> {
    pub fn new(dev: B) -> Result<Raw<B>, Unusable> {
        let bs = dev.block_size();
        if bs == 0 || 4096 % bs != 0 {
            return Err(Unusable::BlockSize(bs));
        }
        let size = dev.blocks().saturating_mul(bs as u64);
        if size < MIN_PARTITION {
            return Err(Unusable::TooSmall(size));
        }
        Ok(Raw { dev, bs: bs as u64 })
    }

    fn read_at(&mut self, off: u64, len: usize) -> Option<Vec<u8>> {
        let mut b = vec![0u8; len];
        self.dev.read(off / self.bs, &mut b).then_some(b)
    }

    /// Writes `bytes` at `off`, flushes, reads them back: `true` only when the bytes
    /// read back are the bytes written.
    fn write_at(&mut self, off: u64, bytes: &[u8]) -> bool {
        if self.dev.read_only() || !self.dev.write(off / self.bs, bytes) || !self.dev.flush() {
            return false;
        }
        self.read_at(off, bytes.len()).as_deref() == Some(bytes)
    }

    /// Writes the probe report (already padded to [`PROBE_BYTES`]).
    pub fn write_probe(&mut self, bytes: &[u8]) -> bool {
        bytes.len() == PROBE_BYTES && self.write_at(PROBE_OFFSET, bytes)
    }

    pub fn read_probe(&mut self) -> Option<Vec<u8>> {
        self.read_at(PROBE_OFFSET, PROBE_BYTES)
    }
}

impl<B: Blocks> RecordStore for Raw<B> {
    fn read_slot(&mut self, slot: usize) -> Result<Vec<u8>, ReadErr> {
        self.read_at(SLOT_OFFSETS[slot], SLOT_BYTES).ok_or(ReadErr::Io)
    }

    fn write_slot(&mut self, slot: usize, bytes: &[u8]) -> bool {
        // The slot's size never changes; the caller reads it back.
        bytes.len() == SLOT_BYTES && !self.dev.read_only() && self.dev.write(SLOT_OFFSETS[slot] / self.bs, bytes) && self.dev.flush()
    }
}

// ---- the loader image header --------------------------------------------------------------

pub const HEADER_MAGIC: &[u8; 8] = b"GLLOADER";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BadHeader {
    Short,
    Magic,
    HeaderCrc,
    Format,
    Flags,
    Size,
}

fn u32_at(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}

/// The 4096 bytes of the header for `image`.
pub fn loader_header(image: &[u8]) -> Vec<u8> {
    let mut h = vec![0u8; 4096];
    h[0..8].copy_from_slice(HEADER_MAGIC);
    h[8..12].copy_from_slice(&1u32.to_le_bytes());
    h[16..24].copy_from_slice(&(image.len() as u64).to_le_bytes());
    h[24..28].copy_from_slice(&crc32(image).to_le_bytes());
    let c = crc32(&h[0..28]);
    h[28..32].copy_from_slice(&c.to_le_bytes());
    h
}

/// The image's size and CRC from a header, as a parent checks it. `part`: the
/// partition's size in bytes.
pub fn check_header(h: &[u8], part: u64) -> Result<(u64, u32), BadHeader> {
    if h.len() < 32 {
        return Err(BadHeader::Short);
    }
    if &h[0..8] != HEADER_MAGIC {
        return Err(BadHeader::Magic);
    }
    if crc32(&h[0..28]) != u32_at(h, 28) {
        return Err(BadHeader::HeaderCrc);
    }
    if u32_at(h, 8) != 1 {
        return Err(BadHeader::Format);
    }
    if u32_at(h, 12) != 0 {
        return Err(BadHeader::Flags);
    }
    let size = u64::from_le_bytes(h[16..24].try_into().unwrap());
    if size == 0 || size > LOADER_MAX || LOADER_IMAGE_OFFSET.checked_add(size).is_none_or(|end| end > part) {
        return Err(BadHeader::Size);
    }
    Ok((size, u32_at(h, 24)))
}

#[cfg(test)]
mod tests;
