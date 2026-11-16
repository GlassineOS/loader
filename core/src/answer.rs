// SPDX-License-Identifier: GPL-2.0-or-later
//! The loader's answer to a parent bootloader (GlassineOS/boot `docs/handback.md`, "The
//! answer", format 1): what it encodes into the UEFI exit data, and the parent's
//! reading of it, which the stand-in parent of the QEMU tests uses and which the ABL's
//! C parser follows.
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::gspl::crc32;

/// The UCS-2 text at the start of the exit data, without its terminating zero.
pub const PREFIX: &str = "GLASSINE-HANDBACK";
/// The prefix's bytes with the terminating zero.
pub const PREFIX_BYTES: usize = (PREFIX.len() + 1) * 2;
/// The largest answer after the prefix.
pub const MAX_ANSWER: usize = 4096;

/// What the parent boots.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Boot {
    Baseline,
    Recovery,
    /// A kernel set: its id and the `_rec` value the record names for it.
    Set { id: String, rec: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Answer {
    pub boot: Boot,
    /// `glassine.deploy.once=<id>` or nothing.
    pub cmdline_add: Option<String>,
    /// Informative.
    pub why: &'static str,
}

impl Answer {
    pub fn new(boot: Boot, why: &'static str) -> Answer {
        Answer { boot, cmdline_add: None, why }
    }

    /// The answer's text with its CRC line.
    pub fn text(&self) -> Vec<u8> {
        let mut t = String::from("format=1\n");
        match &self.boot {
            Boot::Baseline => t.push_str("boot=baseline\n"),
            Boot::Recovery => t.push_str("boot=recovery\n"),
            Boot::Set { id, rec } => t.push_str(&format!("boot=set:{id}\nset_rec={rec}\n")),
        }
        if let Some(c) = &self.cmdline_add {
            t.push_str(&format!("cmdline_add={c}\n"));
        }
        t.push_str(&format!("why={}\n", self.why));
        let c = crc32(t.as_bytes());
        t.push_str(&format!("crc32={c:08x}\n"));
        t.into_bytes()
    }

    /// The whole exit data: the UCS-2 prefix, its zero, the answer.
    pub fn exit_data(&self) -> Vec<u8> {
        let mut b = Vec::new();
        for u in PREFIX.encode_utf16().chain([0u16]) {
            b.extend_from_slice(&u.to_le_bytes());
        }
        b.extend_from_slice(&self.text());
        b
    }
}

/// Why a parent boots its baseline instead (handback.md, "What the parent does with
/// it").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Floor {
    NoData,
    Size,
    Prefix,
    NotText,
    NoCrc,
    Crc,
    NotFormat1,
    NotKeyValue,
    Repeated,
    NoBoot,
    UnknownBoot,
}

fn hex_n(s: &str, n: usize) -> bool {
    s.len() == n && s.bytes().all(|c| matches!(c, b'0'..=b'9' | b'a'..=b'f'))
}

/// The parent's reading of the exit data.
pub fn parse(data: &[u8]) -> Result<Answer, Floor> {
    if data.is_empty() {
        return Err(Floor::NoData);
    }
    if data.len() < PREFIX_BYTES || data.len() > PREFIX_BYTES + MAX_ANSWER {
        return Err(Floor::Size);
    }
    let want: Vec<u8> = PREFIX.encode_utf16().chain([0u16]).flat_map(|u| u.to_le_bytes()).collect();
    if data[..PREFIX_BYTES] != want[..] {
        return Err(Floor::Prefix);
    }
    let t = core::str::from_utf8(&data[PREFIX_BYTES..]).map_err(|_| Floor::NotText)?;
    if !t.ends_with('\n') || t.contains('\r') {
        return Err(Floor::NotText);
    }
    let body_end = t[..t.len() - 1].rfind('\n').map(|i| i + 1).unwrap_or(0);
    let last = &t[body_end..t.len() - 1];
    let crc = last.strip_prefix("crc32=").ok_or(Floor::NoCrc)?;
    if !hex_n(crc, 8) || u32::from_str_radix(crc, 16).ok() != Some(crc32(t[..body_end].as_bytes())) {
        return Err(Floor::Crc);
    }
    let mut seen: Vec<&str> = Vec::new();
    let mut boot = None;
    let mut rec = "";
    let mut add = None;
    for (n, line) in t[..body_end].lines().enumerate() {
        let (k, v) = line.split_once('=').ok_or(Floor::NotKeyValue)?;
        if k.is_empty() {
            return Err(Floor::NotKeyValue);
        }
        if seen.contains(&k) {
            return Err(Floor::Repeated);
        }
        seen.push(k);
        if n == 0 && (k, v) != ("format", "1") {
            return Err(Floor::NotFormat1);
        }
        match k {
            "boot" => boot = Some(v),
            "set_rec" => rec = v,
            "cmdline_add" => add = Some(v),
            _ => {}
        }
    }
    if seen.first() != Some(&"format") {
        return Err(Floor::NotFormat1);
    }
    let boot = match boot.ok_or(Floor::NoBoot)? {
        "baseline" => Boot::Baseline,
        "recovery" => Boot::Recovery,
        v => match v.strip_prefix("set:") {
            Some(id) if hex_n(id, 16) => Boot::Set { id: id.into(), rec: rec.into() },
            _ => return Err(Floor::UnknownBoot),
        },
    };
    // Only the one-shot deployment request may be added; anything else is ignored.
    let cmdline_add = add
        .filter(|a| {
            a.strip_prefix("glassine.deploy.once=").is_some_and(|id| {
                !id.is_empty() && !id.starts_with('.') && id.bytes().all(|c| c.is_ascii_alphanumeric() || b"._-".contains(&c))
            })
        })
        .map(String::from);
    Ok(Answer { boot, cmdline_add, why: "" })
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    /// The bytes the ABL's host test (GlassineOS/abl `glassine/test/handback-test.c`,
    /// `Shared`) checks too: both parsers read the same answers.
    pub(crate) const SHARED: [&str; 2] = [
        "format=1\nboot=recovery\nwhy=limit\ncrc32=a84aa7ed\n",
        "format=1\nboot=set:0123456789abcdef\nset_rec=\nwhy=request\ncrc32=1fee18ad\n",
    ];

    #[test]
    fn shared_vectors() {
        assert_eq!(Answer::new(Boot::Recovery, "limit").text(), SHARED[0].as_bytes());
        let set = Answer::new(Boot::Set { id: "0123456789abcdef".into(), rec: String::new() }, "request");
        assert_eq!(set.text(), SHARED[1].as_bytes());
        assert_eq!(parse(&data(SHARED[0])).unwrap().boot, Boot::Recovery);
        assert_eq!(parse(&data(SHARED[1])).unwrap().boot, set.boot);
    }

    fn data(answer_text: &str) -> Vec<u8> {
        let mut b = Answer::new(Boot::Baseline, "").exit_data()[..PREFIX_BYTES].to_vec();
        b.extend_from_slice(answer_text.as_bytes());
        b
    }

    fn crc_line(body: &str) -> String {
        format!("{body}crc32={:08x}\n", crc32(body.as_bytes()))
    }

    #[test]
    fn every_answer_reads_back() {
        for a in [
            Answer::new(Boot::Baseline, "default"),
            Answer::new(Boot::Recovery, "limit"),
            Answer::new(Boot::Set { id: "0123456789abcdef".into(), rec: "ab".repeat(32) }, "request"),
            Answer { boot: Boot::Baseline, cmdline_add: Some("glassine.deploy.once=d-2".into()), why: "default" },
        ] {
            let got = parse(&a.exit_data()).unwrap();
            assert_eq!((got.boot, got.cmdline_add), (a.boot.clone(), a.cmdline_add.clone()));
        }
    }

    #[test]
    fn the_prefix_is_36_bytes_of_ucs2() {
        let d = Answer::new(Boot::Baseline, "").exit_data();
        assert_eq!(PREFIX_BYTES, 36);
        assert_eq!(&d[0..4], &[b'G', 0, b'L', 0]);
        assert_eq!(&d[34..36], &[0, 0]);
    }

    #[test]
    fn everything_else_is_the_floor() {
        let good = Answer::new(Boot::Recovery, "limit").exit_data();
        assert_eq!(parse(&[]), Err(Floor::NoData));
        assert_eq!(parse(&good[..20]), Err(Floor::Size));
        let mut big = good.clone();
        big.resize(PREFIX_BYTES + MAX_ANSWER + 1, b'\n');
        assert_eq!(parse(&big), Err(Floor::Size));
        let mut p = good.clone();
        p[0] = b'X';
        assert_eq!(parse(&p), Err(Floor::Prefix));
        let mut c = good.clone();
        let i = PREFIX_BYTES + 15; // inside "boot=recovery"
        c[i] ^= 1;
        assert_eq!(parse(&c), Err(Floor::Crc));
        assert_eq!(parse(&data("format=1\nboot=baseline\n")), Err(Floor::NoCrc));
        assert_eq!(parse(&data(&crc_line("format=2\nboot=baseline\n"))), Err(Floor::NotFormat1));
        assert_eq!(parse(&data(&crc_line("boot=baseline\nformat=1\n"))), Err(Floor::NotFormat1));
        assert_eq!(parse(&data(&crc_line("format=1\nboot=baseline\nboot=recovery\n"))), Err(Floor::Repeated));
        assert_eq!(parse(&data(&crc_line("format=1\nwhy=x\n"))), Err(Floor::NoBoot));
        for b in ["reboot", "set:", "set:0123", "set:0123456789ABCDEF", "set:0123456789abcdefg", "Baseline", ""] {
            assert_eq!(parse(&data(&crc_line(&format!("format=1\nboot={b}\n")))), Err(Floor::UnknownBoot), "{b}");
        }
        assert_eq!(parse(&data(&crc_line("format=1\nboot=baseline\r\n"))), Err(Floor::NotText));
        let mut z = data(&crc_line("format=1\nboot=baseline\n"));
        z.push(0);
        assert_eq!(parse(&z), Err(Floor::NotText));
        assert_eq!(parse(&data("\u{0}\u{1}garbage\n")), Err(Floor::NoCrc));
        let _ = vec![0u8];
    }

    #[test]
    fn unknown_keys_are_ignored_and_only_the_deploy_request_is_added() {
        let a = parse(&data(&crc_line("format=1\nboot=baseline\nfuture=1\ncmdline_add=init=/bin/sh\n"))).unwrap();
        assert_eq!((a.boot, a.cmdline_add), (Boot::Baseline, None));
        let a = parse(&data(&crc_line("format=1\nboot=baseline\ncmdline_add=glassine.deploy.once=.x\n"))).unwrap();
        assert_eq!(a.cmdline_add, None);
    }
}
