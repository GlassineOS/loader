// SPDX-License-Identifier: GPL-2.0-or-later
//! The splash store's image format, `format=1` (`boot-partition.md`, "The image
//! format"), and drawing a picture into a display mode (011 FR-006, FR-007).
use alloc::vec::Vec;

pub const HEADER: usize = 64;
/// The largest decoded picture, and the largest payload.
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

/// Checks and decodes one image file. Bytes 56 to 63 of the header are not read.
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
    if u16_at(b, 8) != 1 {
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
    if w == 0 || h == 0 || w > MAX_SIDE || h > MAX_SIDE || (w as u64) * (h as u64) * 4 > MAX_PICTURE {
        return Err(Bad::Limits);
    }
    if b.len() as u64 > MAX_FILE {
        return Err(Bad::Limits);
    }
    if u32_at(b, 28) != 1 {
        return Err(Bad::Layout);
    }
    let payload = &b[HEADER..];
    if crc32(payload) != u32_at(b, 48) {
        return Err(Bad::PayloadCrc);
    }
    let n = (w as usize) * (h as usize);
    let px = match u32_at(b, 36) {
        0 if payload.len() == n * 4 => payload.chunks_exact(4).map(|c| u32_at(c, 0) & 0xff_ffff).collect(),
        1 => unrle(payload, n).ok_or(Bad::Decode)?,
        _ => return Err(Bad::Decode),
    };
    Ok(Kind::Picture(Picture { w, h, mode: (u32_at(b, 20), u32_at(b, 24)), bg: u32_at(b, 32) & 0xff_ffff, px }))
}

/// Run-length: a control byte `c` below 128 is followed by `c`+1 pixels; one of 128 or
/// more by one pixel repeated `c`−126 times. Exactly `n` pixels.
fn unrle(p: &[u8], n: usize) -> Option<Vec<u32>> {
    let mut out = Vec::with_capacity(n);
    let mut i = 0;
    while i < p.len() {
        let c = p[i] as usize;
        i += 1;
        if c < 128 {
            let k = c + 1;
            let lit = p.get(i..i + 4 * k)?;
            if out.len() + k > n {
                return None;
            }
            out.extend(lit.chunks_exact(4).map(|q| u32_at(q, 0) & 0xff_ffff));
            i += 4 * k;
        } else {
            let k = c - 126;
            let v = u32_at(p.get(i..i + 4)?, 0) & 0xff_ffff;
            if out.len() + k > n {
                return None;
            }
            out.resize(out.len() + k, v);
            i += 4;
        }
    }
    (out.len() == n).then_some(out)
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

/// Writes an image file (tests and tools; the loader only reads them).
pub fn encode(kind: u16, w: u32, h: u32, mode: (u32, u32), bg: u32, enc: u32, payload: &[u8]) -> Vec<u8> {
    let mut b = Vec::with_capacity(HEADER + payload.len());
    b.extend_from_slice(b"GLSPLASH");
    b.extend_from_slice(&1u16.to_le_bytes());
    b.extend_from_slice(&kind.to_le_bytes());
    for v in [w, h, mode.0, mode.1, if kind == 1 { 0 } else { 1 }, bg, enc] {
        b.extend_from_slice(&v.to_le_bytes());
    }
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
        assert_eq!(edit(&|b| b[8] = 2, true), Bad::Version);
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
