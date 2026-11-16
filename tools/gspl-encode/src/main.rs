// SPDX-License-Identifier: GPL-2.0-or-later
//! `gspl-encode`: writes a splash image of the boot partition's image format 2
//! (GlassineOS/boot `docs/boot-partition.md`, "The image format") from a picture, and
//! checks one as the loader does. One tool for every writer: the running system (the
//! splash, the override, the hibernate preview), the release images, the tests.
//!
//! ```text
//! gspl-encode [options] IN OUT       IN: a PNM file (P5 grey, P6 RGB, maxval 255), or -
//!   --layout grey8|rgb888|bgrx       the stored pixels (required)
//!   --compression best|fast|none|rle|lz4
//!                                    best (default): the smallest; fast: LZ4, no filter
//!   --filter none|up                 with best: also try it; with a compression: use it
//!   --mode WxH                       the display mode it is made for (default: its size)
//!   --bg RRGGBB                      the background colour (default 000000)
//!   --raw WxH:xrgb8888|rgb888|grey8  IN is raw pixels instead (xrgb8888: 32-bit words
//!                                    0x00RRGGBB, little endian, as a framebuffer holds them)
//! gspl-encode --leave-screen OUT     the instruction to leave the screen (an override)
//! gspl-encode --check FILE           checks FILE as the loader does; prints what it holds
//! ```
//! OUT is written as OUT.tmp and renamed; `-` writes to standard output. The tool
//! writes a file and nothing else: putting it in the splash store is the store
//! writer's work (boot-partition.md, "Writing").
use glassine_loader_core::gspl::{self, Compression, Filter, Kind, Layout, Make};
use std::io::{Read, Write};
use std::process::ExitCode;

const USAGE: &str = "usage: gspl-encode --layout grey8|rgb888|bgrx [--compression best|fast|none|rle|lz4] \
[--filter none|up] [--mode WxH] [--bg RRGGBB] [--raw WxH:xrgb8888|rgb888|grey8] IN OUT\n       \
gspl-encode --leave-screen OUT\n       gspl-encode --check FILE";

fn main() -> ExitCode {
    match run(std::env::args().skip(1).collect()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("gspl-encode: {e}");
            ExitCode::FAILURE
        }
    }
}

fn size(s: &str) -> Result<(u32, u32), String> {
    let (w, h) = s.split_once('x').ok_or_else(|| format!("{s}: not WxH"))?;
    Ok((w.parse().map_err(|_| format!("{s}: not WxH"))?, h.parse().map_err(|_| format!("{s}: not WxH"))?))
}

fn run(args: Vec<String>) -> Result<(), String> {
    let mut layout = None;
    let mut comp: Option<Option<Compression>> = None;
    let mut fast = false;
    let mut filter = None;
    let (mut mode, mut bg, mut raw) = (None, 0u32, None);
    let mut files = Vec::new();
    let mut it = args.into_iter();
    while let Some(a) = it.next() {
        let mut val = || it.next().ok_or_else(|| format!("{a} needs a value\n{USAGE}"));
        match a.as_str() {
            "--layout" => {
                layout = Some(match val()?.as_str() {
                    "grey8" => Layout::Grey8,
                    "rgb888" => Layout::Rgb888,
                    "bgrx" => Layout::Bgrx,
                    v => return Err(format!("--layout {v}: grey8, rgb888 or bgrx")),
                })
            }
            "--compression" => {
                comp = Some(match val()?.as_str() {
                    "best" => None,
                    "fast" => {
                        fast = true;
                        Some(Compression::Lz4)
                    }
                    "none" => Some(Compression::None),
                    "rle" => Some(Compression::Rle),
                    "lz4" => Some(Compression::Lz4),
                    v => return Err(format!("--compression {v}: best, fast, none, rle or lz4")),
                })
            }
            "--filter" => {
                filter = Some(match val()?.as_str() {
                    "none" => Filter::None,
                    "up" => Filter::Up,
                    v => return Err(format!("--filter {v}: none or up")),
                })
            }
            "--mode" => mode = Some(size(&val()?)?),
            "--bg" => bg = u32::from_str_radix(val()?.trim_start_matches('#'), 16).map_err(|_| "--bg: RRGGBB".to_string())?,
            "--raw" => raw = Some(val()?),
            "--leave-screen" => return write(&val()?, &gspl::leave_screen()),
            "--check" => return check(&val()?),
            "-h" | "--help" => {
                println!("{USAGE}");
                return Ok(());
            }
            _ if a.starts_with("--") => return Err(format!("{a}: unknown\n{USAGE}")),
            _ => files.push(a),
        }
    }
    let [inp, out] = <[String; 2]>::try_from(files).map_err(|_| USAGE.to_string())?;
    let layout = layout.ok_or_else(|| format!("--layout is needed\n{USAGE}"))?;
    if fast && filter == Some(Filter::Up) {
        return Err("--compression fast is LZ4 without the filter".into());
    }
    let bytes = read(&inp)?;
    let (px, w, h) = match raw {
        Some(r) => from_raw(&bytes, &r)?,
        None => from_pnm(&bytes)?,
    };
    let how = Make { layout, compression: comp.unwrap_or(None), filter: filter.unwrap_or(if comp.flatten().is_some() { Filter::None } else { Filter::Up }) };
    let img = gspl::make(&px, w, h, mode.unwrap_or((w, h)), bg, how).ok_or_else(|| format!("{w}×{h}: past the format's limits"))?;
    // What the loader would read: a writer never leaves an image it skips.
    gspl::parse(&img).map_err(|e| format!("the image doesn't check: {e:?}"))?;
    write(&out, &img)
}

fn read(path: &str) -> Result<Vec<u8>, String> {
    let mut b = Vec::new();
    if path == "-" {
        std::io::stdin().read_to_end(&mut b).map_err(|e| format!("standard input: {e}"))?;
    } else {
        b = std::fs::read(path).map_err(|e| format!("{path}: {e}"))?;
    }
    Ok(b)
}

fn write(path: &str, b: &[u8]) -> Result<(), String> {
    if path == "-" {
        return std::io::stdout().write_all(b).map_err(|e| format!("standard output: {e}"));
    }
    let tmp = format!("{path}.tmp");
    std::fs::write(&tmp, b).map_err(|e| format!("{tmp}: {e}"))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("{path}: {e}"))
}

fn check(path: &str) -> Result<(), String> {
    let b = read(path)?;
    match gspl::parse(&b).map_err(|e| format!("{path}: skipped by the loader: {e:?}"))? {
        Kind::LeaveScreen => println!("format={} kind=leave-screen", b[8]),
        Kind::Picture(p) => println!(
            "format={} kind=picture size={}x{} mode={}x{} layout={} compression={} filter={} bg={:06x} bytes={}",
            b[8],
            p.w,
            p.h,
            p.mode.0,
            p.mode.1,
            ["", "bgrx", "rgb888", "grey8"][b[28] as usize],
            ["none", "rle", "lz4"][b[36] as usize],
            ["none", "up"][b[38] as usize],
            p.bg,
            b.len()
        ),
    }
    Ok(())
}

/// `WxH:FORMAT` raw pixels as `0x00RRGGBB`.
fn from_raw(b: &[u8], spec: &str) -> Result<(Vec<u32>, u32, u32), String> {
    let (dim, fmt) = spec.split_once(':').ok_or_else(|| format!("--raw {spec}: WxH:FORMAT"))?;
    let (w, h) = size(dim)?;
    let n = (w as usize) * (h as usize);
    let bpp = match fmt {
        "xrgb8888" => 4,
        "rgb888" => 3,
        "grey8" => 1,
        _ => return Err(format!("--raw {spec}: xrgb8888, rgb888 or grey8")),
    };
    if b.len() != n * bpp {
        return Err(format!("--raw {spec}: {} bytes, not {}", b.len(), n * bpp));
    }
    let px = match bpp {
        4 => b.chunks_exact(4).map(|c| u32::from_le_bytes([c[0], c[1], c[2], 0])).collect(),
        3 => b.chunks_exact(3).map(|c| u32::from_be_bytes([0, c[0], c[1], c[2]])).collect(),
        _ => b.iter().map(|&g| g as u32 * 0x01_0101).collect(),
    };
    Ok((px, w, h))
}

/// A binary PNM: P5 (grey) or P6 (RGB), maxval 255, comments allowed in the header.
fn from_pnm(b: &[u8]) -> Result<(Vec<u32>, u32, u32), String> {
    let mut i = 0;
    let mut field = || -> Result<String, String> {
        loop {
            match b.get(i) {
                Some(b'#') => {
                    while b.get(i).is_some_and(|&c| c != b'\n') {
                        i += 1;
                    }
                }
                Some(c) if c.is_ascii_whitespace() => i += 1,
                Some(_) => break,
                None => return Err("the picture ends in its header".into()),
            }
        }
        let s = i;
        while b.get(i).is_some_and(|c| !c.is_ascii_whitespace()) {
            i += 1;
        }
        Ok(String::from_utf8_lossy(&b[s..i]).into_owned())
    };
    let magic = field()?;
    let num = |s: String| s.parse::<u32>().map_err(|_| format!("{s}: not a number in the PNM header"));
    let (w, h, max) = (num(field()?)?, num(field()?)?, num(field()?)?);
    if max != 255 {
        return Err(format!("maxval {max}: only 255"));
    }
    let data = &b[i + 1..];
    let n = (w as usize) * (h as usize);
    let px: Vec<u32> = match magic.as_str() {
        "P5" if data.len() >= n => data[..n].iter().map(|&g| g as u32 * 0x01_0101).collect(),
        "P6" if data.len() >= 3 * n => data[..3 * n].chunks_exact(3).map(|c| u32::from_be_bytes([0, c[0], c[1], c[2]])).collect(),
        "P5" | "P6" => return Err("the picture is shorter than its header says".into()),
        m => return Err(format!("{m}: a PNM file P5 or P6 only (or --raw)")),
    };
    Ok((px, w, h))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pnm_with_comments_and_raw_framebuffer_words() {
        let mut p = b"P6\n# made by a test\n2 1\n255\n".to_vec();
        p.extend_from_slice(&[1, 2, 3, 250, 251, 252]);
        assert_eq!(from_pnm(&p).unwrap(), (vec![0x010203, 0xfafbfc], 2, 1));
        let g = b"P5 2 1 255\n\x00\xff";
        assert_eq!(from_pnm(g).unwrap(), (vec![0, 0xffffff], 2, 1));
        assert!(from_pnm(b"P6 2 1 65535\n").is_err());
        assert!(from_pnm(b"P6 2 2 255\n\x01\x02").is_err(), "short");
        assert!(from_pnm(b"P3 1 1 255\n1 2 3").is_err());
        // xrgb8888: the words 0x00RRGGBB in little endian, as a framebuffer holds them.
        assert_eq!(from_raw(&[3, 2, 1, 9], "1x1:xrgb8888").unwrap(), (vec![0x010203], 1, 1));
        assert_eq!(from_raw(&[1, 2, 3], "1x1:rgb888").unwrap(), (vec![0x010203], 1, 1));
        assert!(from_raw(&[1, 2], "1x1:rgb888").is_err());
    }

    #[test]
    fn writes_what_the_loader_reads() {
        let dir = std::env::temp_dir().join(format!("gspl-encode-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (inp, out) = (dir.join("in.pgm"), dir.join("out.gspl"));
        let mut p = b"P5 4 2 255\n".to_vec();
        p.extend_from_slice(&[0, 0, 0, 0, 9, 9, 9, 200]);
        std::fs::write(&inp, &p).unwrap();
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        for c in ["best", "fast", "none", "rle", "lz4"] {
            run(s(&["--layout", "grey8", "--compression", c, "--mode", "8x4", "--bg", "a0b0c0", inp.to_str().unwrap(), out.to_str().unwrap()])).unwrap();
            let Kind::Picture(pic) = gspl::parse(&std::fs::read(&out).unwrap()).unwrap() else { panic!() };
            assert_eq!(pic.px, vec![0, 0, 0, 0, 0x090909, 0x090909, 0x090909, 0xc8c8c8], "{c}");
            assert_eq!((pic.mode, pic.bg), ((8, 4), 0xa0b0c0));
        }
        assert!(run(s(&["--layout", "grey8", "--compression", "fast", "--filter", "up", inp.to_str().unwrap(), out.to_str().unwrap()])).is_err());
        assert!(run(s(&["--compression", "lz4", inp.to_str().unwrap(), out.to_str().unwrap()])).is_err(), "no layout");
        run(s(&["--leave-screen", out.to_str().unwrap()])).unwrap();
        assert_eq!(gspl::parse(&std::fs::read(&out).unwrap()).unwrap(), Kind::LeaveScreen);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
