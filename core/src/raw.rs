// SPDX-License-Identifier: GPL-2.0-or-later
//! The boot partition's raw form (GlassineOS/boot `docs/boot-partition.md`, "The raw
//! form", format 1): the region table, the regions by name, the loader record's two
//! slots in `record`, the probe report, the splash store and the loader image header.
//! No offset is the loader's own: every region comes from the table. What reaches the
//! disk goes through [`Blocks`], which the loader binary provides with the firmware's
//! block I/O and the host tests with a fake that can cut a write.
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use crate::gspl::{self, crc32};
use crate::plan::{Image, ReadErr, RecordStore, SplashStore};
use crate::record::SLOT_BYTES;

/// The table's size; the primary at offset 0, the backup in the partition's last bytes.
pub const TABLE_BYTES: usize = 4096;
pub const TABLE_MAGIC: &[u8; 8] = b"GLREGION";
/// Entries at 64 + 64 × i, before the CRC at 4092.
pub const MAX_ENTRIES: usize = 62;
/// Every region's head: zeroed first when the region is replaced.
pub const HEAD: usize = 4096;

pub const RECORD: &str = "record";
pub const PROBE: &str = "probe";
pub const LOADER: &str = "loader";
pub const SPLASH_DEFAULT: &str = "splash-default";
pub const SPLASH: &str = "splash";
pub const OVERRIDE: &str = "override";

/// The probe report is at most this long (`docs/handback.md`, "The probe report").
pub const PROBE_BYTES: usize = 65536;
/// The loader image header, at the start of the region `loader`.
pub const LOADER_HEADER: usize = 4096;
pub const LOADER_MAX: u64 = (3 << 20) - 4096;

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
    /// Milliseconds since some start, for the splash store's time limit.
    fn now_ms(&mut self) -> u64 {
        0
    }
}

/// Why the partition can't be used at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unusable {
    /// A block size that is zero or doesn't divide 4096.
    BlockSize(u32),
    /// Too small to hold the two copies of the table.
    TooSmall(u64),
}

// ---- the region table ---------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Region {
    pub name: String,
    pub off: u64,
    pub size: u64,
    pub flags: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BadTable {
    Short,
    Magic,
    Format,
    Crc,
    /// The table was made for a partition of another size.
    Size,
    Count,
    /// Entry `i`: its CRC, name, offset, size, or an overlap or a repeated name.
    Entry(usize),
    /// Bytes that must be zero aren't.
    NotZero,
}

fn u32_at(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}

fn u64_at(b: &[u8], o: usize) -> u64 {
    u64::from_le_bytes(b[o..o + 8].try_into().unwrap())
}

fn good_name(n: &[u8]) -> bool {
    !n.is_empty() && n.len() <= 31 && n.iter().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-')
}

/// The regions of a table, if it is valid for a partition of `part` bytes.
pub fn parse_table(b: &[u8], part: u64) -> Result<Vec<Region>, BadTable> {
    if b.len() < TABLE_BYTES {
        return Err(BadTable::Short);
    }
    let b = &b[..TABLE_BYTES];
    if &b[0..8] != TABLE_MAGIC {
        return Err(BadTable::Magic);
    }
    if crc32(&b[..4092]) != u32_at(b, 4092) {
        return Err(BadTable::Crc);
    }
    if u32_at(b, 8) != 1 {
        return Err(BadTable::Format);
    }
    if u64_at(b, 16) != part || part < 2 * TABLE_BYTES as u64 {
        return Err(BadTable::Size);
    }
    let n = u32_at(b, 12) as usize;
    if n == 0 || n > MAX_ENTRIES {
        return Err(BadTable::Count);
    }
    if b[24..64].iter().chain(&b[64 + 64 * n..4092]).any(|c| *c != 0) {
        return Err(BadTable::NotZero);
    }
    let end = part - TABLE_BYTES as u64;
    let mut out: Vec<Region> = Vec::new();
    for i in 0..n {
        let e = &b[64 + 64 * i..128 + 64 * i];
        let len = e[..32].iter().position(|c| *c == 0).unwrap_or(32);
        let (off, size, flags) = (u64_at(e, 32), u64_at(e, 40), u32_at(e, 48));
        let ok = crc32(&e[..52]) == u32_at(e, 52)
            && e[56..64].iter().all(|c| *c == 0)
            && good_name(&e[..len])
            && e[len..32].iter().all(|c| *c == 0)
            && off % 4096 == 0
            && size % 4096 == 0
            && size > 0
            && off >= TABLE_BYTES as u64
            && off.checked_add(size).is_some_and(|x| x <= end);
        if !ok {
            return Err(BadTable::Entry(i));
        }
        let name = String::from_utf8(e[..len].to_vec()).unwrap();
        if out.iter().any(|r| r.name == name || (off < r.off + r.size && r.off < off + size)) {
            return Err(BadTable::Entry(i));
        }
        out.push(Region { name, off, size, flags });
    }
    Ok(out)
}

/// The 4096 bytes of a table for a partition of `part` bytes (the installer's side;
/// the tests).
pub fn build_table(part: u64, regions: &[(&str, u64, u64)]) -> Vec<u8> {
    let mut b = vec![0u8; TABLE_BYTES];
    b[0..8].copy_from_slice(TABLE_MAGIC);
    b[8..12].copy_from_slice(&1u32.to_le_bytes());
    b[12..16].copy_from_slice(&(regions.len() as u32).to_le_bytes());
    b[16..24].copy_from_slice(&part.to_le_bytes());
    for (i, (name, off, size)) in regions.iter().enumerate() {
        let e = &mut b[64 + 64 * i..128 + 64 * i];
        e[..name.len()].copy_from_slice(name.as_bytes());
        e[32..40].copy_from_slice(&off.to_le_bytes());
        e[40..48].copy_from_slice(&size.to_le_bytes());
        let c = crc32(&e[..52]);
        e[52..56].copy_from_slice(&c.to_le_bytes());
    }
    let c = crc32(&b[..4092]);
    b[4092..].copy_from_slice(&c.to_le_bytes());
    b
}

/// The table of `docs/boot-partition.md`, "Sizes", for a 16 MiB partition.
pub fn layout_16mib() -> [(&'static str, u64, u64); 6] {
    const K: u64 = 1024;
    const M: u64 = 1 << 20;
    [
        (RECORD, 64 * K, 32 * K),
        (PROBE, 128 * K, 64 * K),
        (LOADER, M, 3 * M),
        (SPLASH_DEFAULT, 4 * M, 3 * M),
        (SPLASH, 7 * M, 3 * M),
        (OVERRIDE, 10 * M, 3 * M),
    ]
}

// ---- the partition ------------------------------------------------------------------------

/// Which copy of the table is in use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TableCopy {
    Primary,
    Backup,
}

/// The boot partition in its raw form.
pub struct Raw<B: Blocks> {
    pub dev: B,
    bs: u64,
    size: u64,
    /// The regions, when one copy of the table is valid; none on a partition that
    /// holds nothing.
    pub regions: Option<(TableCopy, Vec<Region>)>,
}

impl<B: Blocks> Raw<B> {
    pub fn new(dev: B) -> Result<Raw<B>, Unusable> {
        let bs = dev.block_size();
        if bs == 0 || 4096 % bs != 0 {
            return Err(Unusable::BlockSize(bs));
        }
        let size = dev.blocks().saturating_mul(bs as u64);
        if size < 2 * TABLE_BYTES as u64 {
            return Err(Unusable::TooSmall(size));
        }
        let mut r = Raw { dev, bs: bs as u64, size, regions: None };
        for (copy, off) in [(TableCopy::Primary, 0), (TableCopy::Backup, size - TABLE_BYTES as u64)] {
            if let Some(t) = r.read_at(off, TABLE_BYTES) {
                if let Ok(regs) = parse_table(&t, size) {
                    r.regions = Some((copy, regs));
                    break;
                }
            }
        }
        Ok(r)
    }

    pub fn size(&self) -> u64 {
        self.size
    }

    /// The region of that name with no flag set: its offset and size.
    pub fn region(&self, name: &str) -> Option<(u64, u64)> {
        let (_, regs) = self.regions.as_ref()?;
        regs.iter().find(|r| r.name == name && r.flags == 0).map(|r| (r.off, r.size))
    }

    fn read_at(&mut self, off: u64, len: usize) -> Option<Vec<u8>> {
        let n = (len as u64).div_ceil(self.bs) * self.bs;
        let mut b = vec![0u8; n as usize];
        if !self.dev.read(off / self.bs, &mut b) {
            return None;
        }
        b.truncate(len);
        Some(b)
    }

    /// Writes `bytes` (padded with zeros to whole blocks) at `off` and flushes.
    fn put(&mut self, off: u64, bytes: &[u8]) -> bool {
        let n = (bytes.len() as u64).div_ceil(self.bs) * self.bs;
        let mut b = bytes.to_vec();
        b.resize(n as usize, 0);
        !self.dev.read_only() && self.dev.write(off / self.bs, &b) && self.dev.flush()
    }

    /// The first `len` bytes of a region.
    pub fn read_region(&mut self, name: &str, len: usize) -> Option<Vec<u8>> {
        let (off, size) = self.region(name)?;
        if len as u64 > size {
            return None;
        }
        self.read_at(off, len)
    }

    /// Zeroes the region's head and reads it back as zero: the region then holds no
    /// item.
    pub fn clear_head(&mut self, name: &str) -> bool {
        let Some((off, size)) = self.region(name) else { return false };
        let n = HEAD.min(size as usize);
        self.put(off, &vec![0u8; n]) && self.read_at(off, n).is_some_and(|b| b.iter().all(|c| *c == 0))
    }

    /// Replaces a region's content (`docs/boot-partition.md`, "Writing a region"): the
    /// head zeroed, the rest written, then the head, then all of it read back. `false`
    /// when the content doesn't fit or any step failed; a content too large writes
    /// nothing.
    pub fn write_region(&mut self, name: &str, content: &[u8]) -> bool {
        let Some((off, size)) = self.region(name) else { return false };
        if content.is_empty() || content.len() as u64 > size || self.dev.read_only() {
            return false;
        }
        if !self.clear_head(name) {
            return false;
        }
        let h = HEAD.min(content.len());
        if content.len() > h && !self.put(off + h as u64, &content[h..]) {
            return false;
        }
        if !self.put(off, &content[..h]) {
            return false;
        }
        self.read_at(off, content.len()).as_deref() == Some(content)
    }

    /// Writes the probe report (`docs/handback.md`, "The probe report").
    pub fn write_probe(&mut self, report: &[u8]) -> bool {
        report.len() <= PROBE_BYTES && self.write_region(PROBE, report)
    }

    pub fn read_probe(&mut self) -> Option<Vec<u8>> {
        let (_, size) = self.region(PROBE)?;
        self.read_region(PROBE, (size as usize).min(PROBE_BYTES))
    }

    fn image_region(which: Image) -> &'static str {
        match which {
            Image::Default => SPLASH_DEFAULT,
            Image::Splash => SPLASH,
            Image::Override => OVERRIDE,
        }
    }

    /// Writes an image of the splash store (the running system's side, 011 FR-019,
    /// FR-020): refused when it is larger than its region.
    pub fn write_image(&mut self, which: Image, file: &[u8]) -> bool {
        self.write_region(Self::image_region(which), file)
    }
}

impl<B: Blocks> RecordStore for Raw<B> {
    fn read_slot(&mut self, slot: usize) -> Result<Vec<u8>, ReadErr> {
        let (off, size) = self.region(RECORD).ok_or(ReadErr::Missing)?;
        if size < 2 * SLOT_BYTES as u64 {
            return Err(ReadErr::Missing);
        }
        self.read_at(off + (slot * SLOT_BYTES) as u64, SLOT_BYTES).ok_or(ReadErr::Io)
    }

    fn write_slot(&mut self, slot: usize, bytes: &[u8]) -> bool {
        let Some((off, size)) = self.region(RECORD) else { return false };
        // The slot's size never changes; the caller reads it back.
        size >= 2 * SLOT_BYTES as u64 && bytes.len() == SLOT_BYTES && self.put(off + (slot * SLOT_BYTES) as u64, bytes)
    }
}

impl<B: Blocks> SplashStore for Raw<B> {
    fn read_image(&mut self, which: Image, max: u64, deadline_ms: Option<u64>) -> Result<Vec<u8>, ReadErr> {
        let name = Self::image_region(which);
        let (_, size) = self.region(name).ok_or(ReadErr::Missing)?;
        let head = self.read_region(name, gspl::HEADER.min(size as usize)).ok_or(ReadErr::Io)?;
        if head.len() < gspl::HEADER || &head[0..8] != b"GLSPLASH" {
            return Err(ReadErr::Missing);
        }
        // The file's length from its header; the header's own CRC is checked by the
        // image's reader.
        let len = u64_at(&head, 40).checked_add(gspl::HEADER as u64).ok_or(ReadErr::TooLarge(u64::MAX))?;
        if len > size || len > max {
            return Err(ReadErr::TooLarge(len));
        }
        if deadline_ms.is_some_and(|d| self.dev.now_ms() > d) {
            return Err(ReadErr::TimedOut);
        }
        self.read_region(name, len as usize).ok_or(ReadErr::Io)
    }

    fn has_override(&mut self) -> bool {
        self.read_region(OVERRIDE, 8).is_some_and(|b| &b[..] == b"GLSPLASH")
    }

    fn remove_override(&mut self) -> bool {
        self.clear_head(OVERRIDE)
    }

    fn now_ms(&mut self) -> u64 {
        self.dev.now_ms()
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

/// The 4096 bytes of the header for `image`.
pub fn loader_header(image: &[u8]) -> Vec<u8> {
    let mut h = vec![0u8; LOADER_HEADER];
    h[0..8].copy_from_slice(HEADER_MAGIC);
    h[8..12].copy_from_slice(&1u32.to_le_bytes());
    h[16..24].copy_from_slice(&(image.len() as u64).to_le_bytes());
    h[24..28].copy_from_slice(&crc32(image).to_le_bytes());
    let c = crc32(&h[0..28]);
    h[28..32].copy_from_slice(&c.to_le_bytes());
    h
}

/// The image's size and CRC from a header, as a parent checks it. `region`: the size
/// of the region `loader` in bytes.
pub fn check_header(h: &[u8], region: u64) -> Result<(u64, u32), BadHeader> {
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
    if size == 0 || size > LOADER_MAX || (LOADER_HEADER as u64).checked_add(size).is_none_or(|end| end > region) {
        return Err(BadHeader::Size);
    }
    Ok((size, u32_at(h, 24)))
}

#[cfg(test)]
mod tests;
