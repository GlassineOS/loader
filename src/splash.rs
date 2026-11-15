//! The splash store's image format (spike draft of 011 FR-004) and drawing it.
//!
//! Header, 64 bytes, little endian:
//!   0  magic "GLSPLASH"      8  u16 version (1)     10 u16 kind (0 image, 1 leave screen)
//!   12 u32 width             16 u32 height          20 u32 mode width  24 u32 mode height
//!   28 u32 pixel layout (1 = XRGB8888, as GOP's BGRX bytes)  32 u32 background 0x00RRGGBB
//!   36 u32 encoding (0 raw, 1 run-length)  40 u64 payload length
//!   48 u32 CRC-32 of the payload   52 u32 CRC-32 of bytes 0..52   56 8 bytes zero
//! Run-length payload: a control byte c; c < 128: c+1 literal pixels (4 bytes each)
//! follow; c >= 128: the next pixel repeats c-126 times (2..=129).
use alloc::vec::Vec;

pub const HEADER: usize = 64;
pub const MAX_BYTES: u64 = 32 << 20;
const MAX_SIDE: u32 = 16384;

#[derive(Debug)]
pub enum Kind {
    Image(Image),
    LeaveScreen,
}

#[derive(Debug)]
pub struct Image {
    pub w: u32,
    pub h: u32,
    pub mode: (u32, u32),
    pub bg: u32,
    pub px: Vec<u32>,
}

#[derive(Debug)]
pub enum Bad {
    Short,
    Magic,
    Version,
    HeaderCrc,
    Length,
    Size,
    Layout,
    PayloadCrc,
    Decode,
}

fn u16_at(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}
fn u32_at(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}

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
    let ver = u16_at(b, 8);
    if ver != 1 {
        return Err(Bad::Version);
    }
    if u16_at(b, 10) == 1 {
        return Ok(Kind::LeaveScreen);
    }
    let (w, h) = (u32_at(b, 12), u32_at(b, 16));
    let plen = u64::from_le_bytes(b[40..48].try_into().unwrap());
    if plen != (b.len() - HEADER) as u64 {
        return Err(Bad::Length);
    }
    if w == 0 || h == 0 || w > MAX_SIDE || h > MAX_SIDE || (w as u64) * (h as u64) * 4 > MAX_BYTES {
        return Err(Bad::Size);
    }
    if u32_at(b, 28) != 1 {
        return Err(Bad::Layout);
    }
    let payload = &b[HEADER..];
    if crc32(payload) != u32_at(b, 48) {
        return Err(Bad::PayloadCrc);
    }
    let n = (w * h) as usize;
    let px = match u32_at(b, 36) {
        0 if payload.len() == n * 4 => payload.chunks_exact(4).map(|c| u32_at(c, 0) & 0xffffff).collect(),
        1 => unrle(payload, n).ok_or(Bad::Decode)?,
        _ => return Err(Bad::Decode),
    };
    Ok(Kind::Image(Image { w, h, mode: (u32_at(b, 20), u32_at(b, 24)), bg: u32_at(b, 32) & 0xffffff, px }))
}

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
            out.extend(lit.chunks_exact(4).map(|q| u32_at(q, 0) & 0xffffff));
            i += 4 * k;
        } else {
            let k = c - 126;
            let v = u32_at(p.get(i..i + 4)?, 0) & 0xffffff;
            if out.len() + k > n {
                return None;
            }
            out.resize(out.len() + k, v);
            i += 4;
        }
    }
    (out.len() == n).then_some(out)
}

/// Draws `img` into a screen buffer of `sw`x`sh`: pixel for pixel when made for this
/// mode (011 FR-006), else scaled to fit, aspect kept, centred on its background
/// (FR-007). Nearest neighbour, integer only.
pub fn compose(img: &Image, sw: u32, sh: u32, screen: &mut [u32]) {
    screen.fill(img.bg);
    let native = img.mode == (sw, sh);
    let (dw, dh) = if native {
        (img.w.min(sw), img.h.min(sh))
    } else if (img.w as u64) * (sh as u64) <= (img.h as u64) * (sw as u64) {
        (((img.w as u64 * sh as u64) / img.h as u64).max(1) as u32, sh)
    } else {
        (sw, ((img.h as u64 * sw as u64) / img.w as u64).max(1) as u32)
    };
    let (ox, oy) = ((sw - dw) / 2, (sh - dh) / 2);
    for y in 0..dh {
        let sy = if native { y } else { (y as u64 * img.h as u64 / dh as u64) as u32 };
        let src = &img.px[(sy * img.w) as usize..((sy + 1) * img.w) as usize];
        let row = &mut screen[((oy + y) * sw + ox) as usize..((oy + y) * sw + ox + dw) as usize];
        if native {
            row.copy_from_slice(&src[..dw as usize]);
        } else {
            for (x, d) in row.iter_mut().enumerate() {
                *d = src[(x as u64 * img.w as u64 / dw as u64) as usize];
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
            c = if c & 1 != 0 { 0xedb88320 ^ (c >> 1) } else { c >> 1 };
            k += 1;
        }
        t[i] = c;
        i += 1;
    }
    t
}
static CRC: [u32; 256] = crc_table();

pub fn crc32(b: &[u8]) -> u32 {
    let mut c = !0u32;
    for &x in b {
        c = CRC[((c ^ x as u32) & 0xff) as usize] ^ (c >> 8);
    }
    !c
}
