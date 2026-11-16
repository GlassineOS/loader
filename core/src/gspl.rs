// SPDX-License-Identifier: GPL-2.0-or-later
//! The splash store's image format (`boot-partition.md`, "The image format"):
//! format 2, with its pixel layouts, compressions and filter, and format 1, which
//! readers keep reading; and drawing a picture into a display mode (011 FR-006, FR-007).
use alloc::vec;
use alloc::vec::Vec;

pub const HEADER: usize = 64;
/// The largest picture in pixels (format 1's 32 MiB at four bytes a pixel), for
/// every layout.
pub const MAX_PIXELS: u64 = 8 << 20;
/// The largest decoded picture in bytes, and the largest payload.
pub const MAX_PICTURE: u64 = 32 << 20;
/// The largest file.
pub const MAX_FILE: u64 = MAX_PICTURE + HEADER as u64;
const MAX_SIDE: u32 = 16384;

#[derive(Debug, PartialEq, Eq)]
pub enum Kind {
    Picture(Picture),
    /// Leave the screen as it is (valid only as the override).
    LeaveScreen,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Picture {
    pub w: u32,
    pub h: u32,
    /// The display mode it was made for.
    pub mode: (u32, u32),
    /// `0x00RRGGBB`.
    pub bg: u32,
    /// `0x00RRGGBB` per pixel, row by row from the top.
    pub px: Vec<u32>,
}

/// How a pixel is stored (header offset 28).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layout {
    /// 1: four bytes, blue, green, red, one unused (format 1's only layout).
    Bgrx = 1,
    /// 2: three bytes, red, green, blue.
    Rgb888 = 2,
    /// 3: one byte of grey, 0 black to 255 white.
    Grey8 = 3,
}

impl Layout {
    pub fn from_u32(v: u32) -> Option<Layout> {
        match v {
            1 => Some(Layout::Bgrx),
            2 => Some(Layout::Rgb888),
            3 => Some(Layout::Grey8),
            _ => None,
        }
    }
    /// Bytes a pixel.
    pub fn bytes(self) -> usize {
        match self {
            Layout::Bgrx => 4,
            Layout::Rgb888 => 3,
            Layout::Grey8 => 1,
        }
    }
    /// One stored pixel as `0x00RRGGBB`.
    fn rgb(self, p: &[u8]) -> u32 {
        match self {
            Layout::Bgrx => u32::from_le_bytes([p[0], p[1], p[2], 0]),
            Layout::Rgb888 => u32::from_be_bytes([0, p[0], p[1], p[2]]),
            Layout::Grey8 => (p[0] as u32) * 0x01_0101,
        }
    }
    /// `0x00RRGGBB` as this layout's bytes (writers; grey is the integer luma
    /// `(77 R + 150 G + 29 B + 128) / 256`).
    pub fn store(self, rgb: u32, out: &mut Vec<u8>) {
        let [_, r, g, b] = rgb.to_be_bytes();
        match self {
            Layout::Bgrx => out.extend_from_slice(&[b, g, r, 0]),
            Layout::Rgb888 => out.extend_from_slice(&[r, g, b]),
            Layout::Grey8 => out.push(((77 * r as u32 + 150 * g as u32 + 29 * b as u32 + 128) >> 8) as u8),
        }
    }
}

/// How the payload is compressed (header offset 36, two bytes).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Compression {
    /// 0: the stored pixels as they are.
    None = 0,
    /// 1: run-length, by pixels of the layout.
    Rle = 1,
    /// 2: one LZ4 block.
    Lz4 = 2,
}

impl Compression {
    pub fn from_u16(v: u16) -> Option<Compression> {
        match v {
            0 => Some(Compression::None),
            1 => Some(Compression::Rle),
            2 => Some(Compression::Lz4),
            _ => None,
        }
    }
}

/// What is done to the stored pixels before they are compressed (header offset 38,
/// two bytes).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Filter {
    /// 0: nothing.
    None = 0,
    /// 1: each byte less the byte one row above (modulo 256); the first row as it is.
    Up = 1,
}

impl Filter {
    pub fn from_u16(v: u16) -> Option<Filter> {
        match v {
            0 => Some(Filter::None),
            1 => Some(Filter::Up),
            _ => None,
        }
    }
}

/// The first check an image failed, in the order the format gives them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bad {
    Short,
    Magic,
    HeaderCrc,
    Version,
    Kind,
    Length,
    Limits,
    Layout,
    /// A compression or a filter this format doesn't name.
    Encoding,
    PayloadCrc,
    Decode,
}

fn u16_at(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}
fn u32_at(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}
fn u64_at(b: &[u8], o: usize) -> u64 {
    (u32_at(b, o) as u64) | ((u32_at(b, o + 4) as u64) << 32)
}

/// Checks and decodes one image file, format 1 or 2. Bytes 56 to 63 of the header are
/// not read.
pub fn parse(b: &[u8]) -> Result<Kind, Bad> {
    if b.len() < HEADER {
        return Err(Bad::Short);
    }
    if &b[0..8] != b"GLSPLASH" {
        return Err(Bad::Magic);
    }
    if crc32(&b[0..52]) != u32_at(b, 52) {
        return Err(Bad::HeaderCrc);
    }
    let version = u16_at(b, 8);
    if version != 1 && version != 2 {
        return Err(Bad::Version);
    }
    let plen = u64_at(b, 40);
    match u16_at(b, 10) {
        0 => {}
        1 => {
            // Leave the screen: the other picture fields are zero and no payload follows.
            if b[12..52].iter().any(|x| *x != 0) {
                return Err(Bad::Kind);
            }
            if b.len() != HEADER {
                return Err(Bad::Length);
            }
            return Ok(Kind::LeaveScreen);
        }
        _ => return Err(Bad::Kind),
    }
    if plen != (b.len() - HEADER) as u64 {
        return Err(Bad::Length);
    }
    let (w, h) = (u32_at(b, 12), u32_at(b, 16));
    if w == 0 || h == 0 || w > MAX_SIDE || h > MAX_SIDE || (w as u64) * (h as u64) > MAX_PIXELS {
        return Err(Bad::Limits);
    }
    if b.len() as u64 > MAX_FILE {
        return Err(Bad::Limits);
    }
    let layout = Layout::from_u32(u32_at(b, 28)).filter(|l| version == 2 || *l == Layout::Bgrx).ok_or(Bad::Layout)?;
    // Format 1 has one field of four bytes here, the encoding 0 or 1; read as format
    // 2's two fields, that is the same compression and the filter 0. Format 1 checks
    // it when it decodes.
    let (comp, filter) = (Compression::from_u16(u16_at(b, 36)), Filter::from_u16(u16_at(b, 38)));
    if version == 2 && (comp.is_none() || filter.is_none()) {
        return Err(Bad::Encoding);
    }
    let payload = &b[HEADER..];
    if crc32(payload) != u32_at(b, 48) {
        return Err(Bad::PayloadCrc);
    }
    let (comp, filter) = match (version, comp, filter) {
        (2, Some(c), Some(f)) => (c, f),
        (1, Some(c @ (Compression::None | Compression::Rle)), Some(Filter::None)) => (c, Filter::None),
        _ => return Err(Bad::Decode),
    };
    let n = (w as usize) * (h as usize);
    let stored = decode(payload, layout, comp, filter, w as usize, n).ok_or(Bad::Decode)?;
    let px = stored.chunks_exact(layout.bytes()).map(|p| layout.rgb(p)).collect();
    Ok(Kind::Picture(Picture { w, h, mode: (u32_at(b, 20), u32_at(b, 24)), bg: u32_at(b, 32) & 0xff_ffff, px }))
}

/// The stored pixels of a payload: exactly `n` pixels of `layout`, unfiltered.
fn decode(p: &[u8], layout: Layout, comp: Compression, filter: Filter, w: usize, n: usize) -> Option<Vec<u8>> {
    let total = n * layout.bytes();
    let mut out = match comp {
        Compression::None => (p.len() == total).then(|| p.to_vec())?,
        Compression::Rle => unrle(p, layout.bytes(), total)?,
        Compression::Lz4 => {
            let mut out = vec![0u8; total];
            (lz4_flex::block::decompress_into(p, &mut out).ok()? == total).then_some(out)?
        }
    };
    if filter == Filter::Up {
        let stride = w * layout.bytes();
        for i in stride..total {
            out[i] = out[i].wrapping_add(out[i - stride]);
        }
    }
    Some(out)
}

/// Run-length by pixels of `u` bytes: a control byte `c` below 128 is followed by
/// `c`+1 pixels; one of 128 or more by one pixel repeated `c`−126 times. Exactly
/// `total` bytes.
fn unrle(p: &[u8], u: usize, total: usize) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(total);
    let mut i = 0;
    while i < p.len() {
        let c = p[i] as usize;
        i += 1;
        if c < 128 {
            let k = (c + 1) * u;
            let lit = p.get(i..i + k)?;
            if out.len() + k > total {
                return None;
            }
            out.extend_from_slice(lit);
            i += k;
        } else {
            let k = (c - 126) * u;
            let v = p.get(i..i + u)?;
            if out.len() + k > total {
                return None;
            }
            for _ in 0..c - 126 {
                out.extend_from_slice(v);
            }
            i += u;
        }
    }
    (out.len() == total).then_some(out)
}

/// Draws `pic` into a screen of `sw`×`sh` (`0x00RRGGBB`, row by row). Made for this
/// mode: pixel for pixel, centred on its background when smaller (011 FR-006). Made for
/// another mode: scaled to fit with its aspect ratio kept, centred on its background
/// (FR-007), nearest neighbour.
pub fn compose(pic: &Picture, sw: u32, sh: u32, screen: &mut [u32]) {
    screen.fill(pic.bg);
    if sw == 0 || sh == 0 {
        return;
    }
    let native = pic.mode == (sw, sh);
    let (dw, dh) = if native {
        (pic.w.min(sw), pic.h.min(sh))
    } else if (pic.w as u64) * (sh as u64) <= (pic.h as u64) * (sw as u64) {
        ((((pic.w as u64) * (sh as u64)) / pic.h as u64).max(1) as u32, sh)
    } else {
        (sw, ((((pic.h as u64) * (sw as u64)) / pic.w as u64).max(1) as u32))
    };
    let (ox, oy) = ((sw - dw) / 2, (sh - dh) / 2);
    for y in 0..dh {
        let sy = if native { y } else { ((y as u64) * (pic.h as u64) / (dh as u64)) as u32 };
        let src = &pic.px[(sy * pic.w) as usize..((sy + 1) * pic.w) as usize];
        let at = ((oy + y) * sw + ox) as usize;
        let row = &mut screen[at..at + dw as usize];
        if native {
            row.copy_from_slice(&src[..dw as usize]);
        } else {
            for (x, d) in row.iter_mut().enumerate() {
                *d = src[((x as u64) * (pic.w as u64) / (dw as u64)) as usize];
            }
        }
    }
}

const fn crc_table() -> [u32; 256] {
    let mut t = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut c = i as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 { 0xedb8_8320 ^ (c >> 1) } else { c >> 1 };
            k += 1;
        }
        t[i] = c;
        i += 1;
    }
    t
}
static CRC: [u32; 256] = crc_table();

/// CRC-32 of zlib and Ethernet.
pub fn crc32(b: &[u8]) -> u32 {
    let mut c = !0u32;
    for &x in b {
        c = CRC[((c ^ x as u32) & 0xff) as usize] ^ (c >> 8);
    }
    !c
}

/// Writes a format 1 image file (tests; the loader only reads them).
pub fn encode(kind: u16, w: u32, h: u32, mode: (u32, u32), bg: u32, enc: u32, payload: &[u8]) -> Vec<u8> {
    let layout = if kind == 1 { 0 } else { 1 };
    header(1, kind, w, h, mode, layout, bg, [enc as u16, (enc >> 16) as u16], payload)
}

/// What a writer makes a format 2 picture of.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Make {
    pub layout: Layout,
    /// `None`: the smallest of the compressions (with and without the filter); a
    /// writer that must be fast names one.
    pub compression: Option<Compression>,
    pub filter: Filter,
}

/// Writes a format 2 picture of `px` (`0x00RRGGBB`, `w`×`h`, row by row) made for
/// `mode`, or `None` past the format's limits. With `compression: None` it tries
/// each compression with the filter given and without it, and keeps the smallest
/// payload (`none` when nothing is smaller).
pub fn make(px: &[u32], w: u32, h: u32, mode: (u32, u32), bg: u32, how: Make) -> Option<Vec<u8>> {
    if w == 0 || h == 0 || w > MAX_SIDE || h > MAX_SIDE || (w as u64) * (h as u64) > MAX_PIXELS {
        return None;
    }
    if px.len() != (w as usize) * (h as usize) {
        return None;
    }
    let mut stored = Vec::with_capacity(px.len() * how.layout.bytes());
    for &p in px {
        how.layout.store(p, &mut stored);
    }
    let stride = w as usize * how.layout.bytes();
    let tries: &[(Compression, Filter)] = match how.compression {
        Some(c) => &[(c, how.filter)],
        None if how.filter == Filter::Up => &[
            (Compression::Lz4, Filter::None),
            (Compression::Lz4, Filter::Up),
            (Compression::Rle, Filter::None),
        ],
        None => &[(Compression::Lz4, Filter::None), (Compression::Rle, Filter::None)],
    };
    let mut best: Option<(Vec<u8>, Compression, Filter)> = None;
    for &(c, f) in tries {
        let p = compress(&stored, how.layout, stride, c, f);
        if best.as_ref().is_none_or(|b| p.len() < b.0.len()) {
            best = Some((p, c, f));
        }
    }
    let (mut payload, mut c, mut f) = best?;
    if how.compression.is_none() && payload.len() >= stored.len() {
        (payload, c, f) = (stored, Compression::None, Filter::None);
    }
    if payload.len() as u64 > MAX_PICTURE {
        return None;
    }
    Some(header(2, 0, w, h, mode, how.layout as u32, bg & 0xff_ffff, [c as u16, f as u16], &payload))
}

/// The instruction to leave the screen as it is, format 2.
pub fn leave_screen() -> Vec<u8> {
    header(2, 1, 0, 0, (0, 0), 0, 0, [0, 0], &[])
}

/// The payload of `stored` (pixels of `layout`, rows of `stride` bytes).
pub fn compress(stored: &[u8], layout: Layout, stride: usize, c: Compression, f: Filter) -> Vec<u8> {
    let filtered;
    let src = match f {
        Filter::None => stored,
        Filter::Up => {
            let mut v = stored.to_vec();
            for i in (stride..v.len()).rev() {
                v[i] = v[i].wrapping_sub(stored[i - stride]);
            }
            filtered = v;
            &filtered
        }
    };
    match c {
        Compression::None => src.to_vec(),
        Compression::Rle => rle(src, layout.bytes()),
        Compression::Lz4 => lz4_flex::block::compress(src),
    }
}

/// Run-length by pixels of `u` bytes (the reverse of [`unrle`]).
fn rle(b: &[u8], u: usize) -> Vec<u8> {
    let n = b.len() / u;
    let px = |i: usize| &b[i * u..(i + 1) * u];
    let mut out = Vec::new();
    let lit = |out: &mut Vec<u8>, mut s: usize, e: usize| {
        while s < e {
            let k = (e - s).min(128);
            out.push((k - 1) as u8);
            out.extend_from_slice(&b[s * u..(s + k) * u]);
            s += k;
        }
    };
    let (mut i, mut from) = (0, 0);
    while i < n {
        let mut j = i + 1;
        while j < n && j - i < 129 && px(j) == px(i) {
            j += 1;
        }
        if j - i >= 2 {
            lit(&mut out, from, i);
            out.push((j - i + 126) as u8);
            out.extend_from_slice(px(i));
            i = j;
            from = i;
        } else {
            i += 1;
        }
    }
    lit(&mut out, from, n);
    out
}

#[allow(clippy::too_many_arguments)]
fn header(version: u16, kind: u16, w: u32, h: u32, mode: (u32, u32), layout: u32, bg: u32, enc: [u16; 2], payload: &[u8]) -> Vec<u8> {
    let mut b = Vec::with_capacity(HEADER + payload.len());
    b.extend_from_slice(b"GLSPLASH");
    b.extend_from_slice(&version.to_le_bytes());
    b.extend_from_slice(&kind.to_le_bytes());
    for v in [w, h, mode.0, mode.1, layout, bg] {
        b.extend_from_slice(&v.to_le_bytes());
    }
    b.extend_from_slice(&enc[0].to_le_bytes());
    b.extend_from_slice(&enc[1].to_le_bytes());
    b.extend_from_slice(&(payload.len() as u64).to_le_bytes());
    b.extend_from_slice(&(if payload.is_empty() && kind == 1 { 0 } else { crc32(payload) }).to_le_bytes());
    let hc = crc32(&b[0..52]);
    b.extend_from_slice(&hc.to_le_bytes());
    b.extend_from_slice(&[0; 8]);
    b.extend_from_slice(payload);
    b
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    fn raw(px: &[u32]) -> Vec<u8> {
        px.iter().flat_map(|p| p.to_le_bytes()).collect()
    }

    fn fix_header(b: &mut [u8]) {
        let c = crc32(&b[0..52]);
        b[52..56].copy_from_slice(&c.to_le_bytes());
    }

    #[test]
    fn crc32_is_zlibs() {
        assert_eq!(crc32(b"123456789"), 0xcbf4_3926);
        assert_eq!(crc32(b""), 0);
    }

    #[test]
    fn raw_and_run_length_decode() {
        let px = [0x0011_2233, 0xff44_5566, 7, 8];
        let k = parse(&encode(0, 2, 2, (2, 2), 0x0abcdef, 0, &raw(&px))).unwrap();
        let Kind::Picture(p) = k else { panic!() };
        assert_eq!(p.px, vec![0x112233, 0x445566, 7, 8], "the unused byte is ignored");
        assert_eq!(p.bg, 0x0abcdef & 0xffffff);
        // one literal, then a run of 3 (130 = 3 repeats).
        let rle = [0, 1, 0, 0, 0, 129, 2, 0, 0, 0];
        let Kind::Picture(p) = parse(&encode(0, 2, 2, (2, 2), 0, 1, &rle)).unwrap() else { panic!() };
        assert_eq!(p.px, vec![1, 2, 2, 2]);
        let max_run = [255u8, 9, 0, 0, 0];
        let Kind::Picture(p) = parse(&encode(0, 129, 1, (129, 1), 0, 1, &max_run)).unwrap() else { panic!() };
        assert_eq!(p.px.len(), 129);
    }

    #[test]
    fn leave_screen_is_only_the_bare_header() {
        let ls = encode(1, 0, 0, (0, 0), 0, 0, &[]);
        assert_eq!(parse(&ls).unwrap(), Kind::LeaveScreen);
        let mut b = ls.clone();
        b[12] = 1;
        fix_header(&mut b);
        assert_eq!(parse(&b).unwrap_err(), Bad::Kind);
        let mut b = ls.clone();
        b.push(0);
        assert_eq!(parse(&b).unwrap_err(), Bad::Length);
        let mut b = ls.clone();
        b[56..64].copy_from_slice(&[0xaa; 8]);
        assert_eq!(parse(&b).unwrap(), Kind::LeaveScreen, "bytes 56..64 are ignored");
    }

    #[test]
    fn a_damaged_image_fails_its_first_check() {
        let good = encode(0, 2, 1, (2, 1), 0, 0, &raw(&[1, 2]));
        assert!(parse(&good).is_ok());
        let edit = |f: &dyn Fn(&mut Vec<u8>), fix: bool| {
            let mut b = good.clone();
            f(&mut b);
            if fix {
                fix_header(&mut b);
            }
            parse(&b).unwrap_err()
        };
        assert_eq!(parse(&good[..63]).unwrap_err(), Bad::Short);
        assert_eq!(edit(&|b| b[0] = b'X', false), Bad::Magic);
        assert_eq!(edit(&|b| b[12] = 3, false), Bad::HeaderCrc);
        assert_eq!(edit(&|b| b[8] = 3, true), Bad::Version);
        assert_eq!(edit(&|b| b[10] = 2, true), Bad::Kind);
        assert_eq!(edit(&|b| b.push(0), false), Bad::Length);
        assert_eq!(edit(&|b| b.truncate(70), false), Bad::Length);
        assert_eq!(edit(&|b| b[12..16].copy_from_slice(&0u32.to_le_bytes()), true), Bad::Limits);
        assert_eq!(edit(&|b| b[12..16].copy_from_slice(&16385u32.to_le_bytes()), true), Bad::Limits);
        assert_eq!(edit(&|b| { b[12..16].copy_from_slice(&16384u32.to_le_bytes()); b[16..20].copy_from_slice(&16384u32.to_le_bytes()); }, true), Bad::Limits);
        assert_eq!(edit(&|b| b[28] = 2, true), Bad::Layout);
        assert_eq!(edit(&|b| b[64] ^= 1, false), Bad::PayloadCrc);
        assert_eq!(edit(&|b| b[36] = 7, true), Bad::Decode);
        let mut b = good.clone();
        b[56..64].copy_from_slice(&[0xff; 8]);
        assert!(parse(&b).is_ok(), "bytes 56..64 are ignored");
        // A raw payload of the wrong size, and run-length that doesn't give exactly w*h.
        assert_eq!(parse(&encode(0, 2, 1, (2, 1), 0, 0, &raw(&[1]))).unwrap_err(), Bad::Decode);
        assert_eq!(parse(&encode(0, 2, 2, (2, 2), 0, 1, &[129, 1, 0, 0, 0])).unwrap_err(), Bad::Decode, "3 pixels");
        assert_eq!(parse(&encode(0, 2, 2, (2, 2), 0, 1, &[131, 1, 0, 0, 0])).unwrap_err(), Bad::Decode, "5 pixels");
        assert_eq!(parse(&encode(0, 2, 2, (2, 2), 0, 1, &[3, 1, 0, 0])).unwrap_err(), Bad::Decode, "cut literal");
        assert_eq!(parse(&encode(0, 2, 2, (2, 2), 0, 1, &[])).unwrap_err(), Bad::Decode);
    }

    const LAYOUTS: [Layout; 3] = [Layout::Bgrx, Layout::Rgb888, Layout::Grey8];
    const COMPS: [Compression; 3] = [Compression::None, Compression::Rle, Compression::Lz4];
    const FILTERS: [Filter; 2] = [Filter::None, Filter::Up];

    /// A picture with runs, edges and noise, so that every codec has work.
    fn sample(w: u32, h: u32, seed: u32) -> Vec<u32> {
        let mut x = seed | 1;
        (0..w * h)
            .map(|i| {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                let (cx, cy) = (i % w, i / w);
                match (cx / 5 + cy / 3) % 4 {
                    0 => 0x00f4_f2ec,
                    1 => x & 0xff_ffff,
                    2 => (cx * 7) << 16 | (cy * 3) << 8 | 0x40,
                    _ => 0x0010_2030,
                }
            })
            .collect()
    }

    fn as_stored(px: &[u32], l: Layout) -> Vec<u32> {
        let mut v = Vec::new();
        for &p in px {
            l.store(p, &mut v);
        }
        v.chunks_exact(l.bytes()).map(|c| l.rgb(c)).collect()
    }

    /// Rewrites the header's lengths and CRCs after the payload was changed.
    fn refit(b: &mut Vec<u8>) {
        let n = (b.len() - HEADER) as u64;
        b[40..48].copy_from_slice(&n.to_le_bytes());
        let c = crc32(&b[HEADER..]);
        b[48..52].copy_from_slice(&c.to_le_bytes());
        fix_header(b);
    }

    #[test]
    fn every_layout_compression_and_filter_round_trips() {
        let (w, h) = (37, 11);
        let px = sample(w, h, 7);
        for l in LAYOUTS {
            for c in COMPS {
                for f in FILTERS {
                    let b = make(&px, w, h, (w, h), 0x123456, Make { layout: l, compression: Some(c), filter: f }).unwrap();
                    assert_eq!(u16_at(&b, 8), 2);
                    assert_eq!((u32_at(&b, 28), u16_at(&b, 36), u16_at(&b, 38)), (l as u32, c as u16, f as u16));
                    let Kind::Picture(p) = parse(&b).unwrap() else { panic!() };
                    assert_eq!(p.px, as_stored(&px, l), "{l:?} {c:?} {f:?}");
                    assert_eq!((p.w, p.h, p.mode, p.bg), (w, h, (w, h), 0x123456));
                }
            }
        }
    }

    #[test]
    fn grey_is_the_integer_luma_and_drawn_as_grey() {
        let px = [0x00ff_ffff, 0, 0x00ff_0000, 0x0000_ff00, 0x0000_00ff];
        let b = make(&px, 5, 1, (5, 1), 0, Make { layout: Layout::Grey8, compression: Some(Compression::None), filter: Filter::None }).unwrap();
        assert_eq!(&b[HEADER..], &[255, 0, 77, 149, 29]);
        let Kind::Picture(p) = parse(&b).unwrap() else { panic!() };
        assert_eq!(p.px, vec![0xffffff, 0, 0x4d4d4d, 0x959595, 0x1d1d1d]);
    }

    #[test]
    fn a_writer_keeps_the_smallest_and_none_when_nothing_is_smaller() {
        let flat = vec![0x00f4_f2ec; 64 * 64];
        let b = make(&flat, 64, 64, (64, 64), 0, Make { layout: Layout::Rgb888, compression: None, filter: Filter::Up }).unwrap();
        assert!(b.len() < 200, "a flat picture compresses: {}", b.len());
        assert!(parse(&b).is_ok());
        let mut x = 99u32;
        let noise: Vec<u32> = (0..64 * 64)
            .map(|_| {
                x = x.wrapping_mul(1_103_515_245).wrapping_add(12345);
                x >> 8
            })
            .collect();
        let b = make(&noise, 64, 64, (64, 64), 0, Make { layout: Layout::Bgrx, compression: None, filter: Filter::Up }).unwrap();
        assert_eq!((u16_at(&b, 36), u16_at(&b, 38)), (0, 0), "noise is stored as it is");
        assert_eq!(b.len(), HEADER + 64 * 64 * 4);
        assert!(make(&flat, 64, 63, (64, 64), 0, Make { layout: Layout::Grey8, compression: None, filter: Filter::None }).is_none(), "wrong count");
    }

    #[test]
    fn format_2_checks_in_order() {
        let px = sample(4, 3, 1);
        let good = make(&px, 4, 3, (4, 3), 0, Make { layout: Layout::Rgb888, compression: Some(Compression::Lz4), filter: Filter::Up }).unwrap();
        assert!(parse(&good).is_ok());
        let edit = |f: &dyn Fn(&mut Vec<u8>)| {
            let mut b = good.clone();
            f(&mut b);
            fix_header(&mut b);
            parse(&b).unwrap_err()
        };
        assert_eq!(edit(&|b| b[8] = 3), Bad::Version);
        assert_eq!(edit(&|b| b[8] = 0), Bad::Version);
        assert_eq!(edit(&|b| b[28] = 0), Bad::Layout);
        assert_eq!(edit(&|b| b[28] = 4), Bad::Layout);
        assert_eq!(edit(&|b| b[36] = 3), Bad::Encoding);
        assert_eq!(edit(&|b| b[38] = 2), Bad::Encoding);
        assert_eq!(edit(&|b| { b[36] = 3; b[64] ^= 1 }), Bad::Encoding, "the encoding before the payload's CRC");
        assert_eq!(edit(&|b| b[64] ^= 1), Bad::PayloadCrc);
        // The pixel limit holds for every layout, before anything is decoded.
        let over = |l: Layout| {
            let mut b = make(&[0], 1, 1, (1, 1), 0, Make { layout: l, compression: Some(Compression::Lz4), filter: Filter::None }).unwrap();
            b[12..16].copy_from_slice(&8192u32.to_le_bytes());
            b[16..20].copy_from_slice(&1025u32.to_le_bytes());
            fix_header(&mut b);
            parse(&b).unwrap_err()
        };
        for l in LAYOUTS {
            assert_eq!(over(l), Bad::Limits, "{l:?}");
        }
        // Leave the screen in format 2.
        assert_eq!(parse(&leave_screen()).unwrap(), Kind::LeaveScreen);
    }

    #[test]
    fn format_1_reads_as_before_and_knows_no_more() {
        let px = [1u32, 2, 3, 4];
        let raw: Vec<u8> = px.iter().flat_map(|p| p.to_le_bytes()).collect();
        let mut b = encode(0, 2, 2, (2, 2), 0, 0, &raw);
        assert!(parse(&b).is_ok());
        b[28] = 3; // grey8 is format 2's
        fix_header(&mut b);
        assert_eq!(parse(&b).unwrap_err(), Bad::Layout);
        for enc in [2u32, 0x0001_0000, 0x0001_0001] {
            assert_eq!(parse(&encode(0, 2, 2, (2, 2), 0, enc, &raw)).unwrap_err(), Bad::Decode, "encoding {enc:#x}");
        }
    }

    #[test]
    fn cut_streams_of_every_codec_fail_to_decode() {
        let (w, h) = (13, 7);
        let px = sample(w, h, 3);
        for l in LAYOUTS {
            for c in COMPS {
                for f in FILTERS {
                    let good = make(&px, w, h, (w, h), 0, Make { layout: l, compression: Some(c), filter: f }).unwrap();
                    for cut in HEADER..good.len() {
                        let mut b = good[..cut].to_vec();
                        refit(&mut b);
                        assert_eq!(parse(&b).unwrap_err(), Bad::Decode, "{l:?} {c:?} {f:?} cut at {cut}");
                    }
                    // One byte too many.
                    let mut b = good.clone();
                    b.push(0);
                    refit(&mut b);
                    assert_eq!(parse(&b).unwrap_err(), Bad::Decode, "{l:?} {c:?} {f:?} one more byte");
                }
            }
        }
    }

    #[test]
    fn damaged_and_random_streams_never_panic() {
        // Cheap fuzzing: every byte of valid streams flipped, and random payloads, with
        // their CRCs made to match so that the decoders see them.
        let mut x = 0x2545_f491u32;
        let mut next = || {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            x
        };
        let (w, h) = (9, 5);
        let px = sample(w, h, 11);
        for l in LAYOUTS {
            for c in COMPS {
                for f in FILTERS {
                    let good = make(&px, w, h, (w, h), 0, Make { layout: l, compression: Some(c), filter: f }).unwrap();
                    for i in HEADER..good.len() {
                        for bit in [0x01u8, 0x80, 0xff] {
                            let mut b = good.clone();
                            b[i] ^= bit;
                            refit(&mut b);
                            if let Ok(Kind::Picture(p)) = parse(&b) {
                                assert_eq!(p.px.len(), (w * h) as usize);
                            }
                        }
                    }
                    for _ in 0..300 {
                        let n = (next() % 200) as usize;
                        let mut b = good[..HEADER].to_vec();
                        b.extend((0..n).map(|_| next() as u8));
                        refit(&mut b);
                        if let Ok(Kind::Picture(p)) = parse(&b) {
                            assert_eq!(p.px.len(), (w * h) as usize);
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn drawing_native_scaled_and_centred() {
        // Native and smaller: centred on the background.
        let p = Picture { w: 2, h: 1, mode: (4, 3), bg: 9, px: vec![1, 2] };
        let mut s = vec![0; 12];
        compose(&p, 4, 3, &mut s);
        assert_eq!(s, vec![9, 9, 9, 9, 9, 1, 2, 9, 9, 9, 9, 9]);
        // Another mode: 2x1 into 4x4 fits as 4x2, centred vertically.
        let p = Picture { w: 2, h: 1, mode: (2, 1), bg: 0, px: vec![1, 2] };
        let mut s = vec![5; 16];
        compose(&p, 4, 4, &mut s);
        assert_eq!(s, vec![0, 0, 0, 0, 1, 1, 2, 2, 1, 1, 2, 2, 0, 0, 0, 0]);
        // Tall into wide: 1x2 into 4x2 fits as 1x2, centred horizontally.
        let p = Picture { w: 1, h: 2, mode: (1, 2), bg: 0, px: vec![1, 2] };
        let mut s = vec![5; 8];
        compose(&p, 4, 2, &mut s);
        assert_eq!(s, vec![0, 1, 0, 0, 0, 2, 0, 0]);
    }
}
