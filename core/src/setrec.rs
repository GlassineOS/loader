// SPDX-License-Identifier: GPL-2.0-or-later
//! A kernel set's record, `set.rec` (`boot-partition.md`, "A kernel set"), and the
//! checks the loader makes before it reads the set's files.
use alloc::string::{String, ToString};

use crate::record;

/// The longest command line of a set: room for the one key the loader may add.
pub const CMDLINE_MAX: usize = 1920;
/// The key the loader may add, and only that one (010 FR-042).
pub const DEPLOY_ONCE: &str = "glassine.deploy.once=";
/// The largest `set.rec` read.
pub const SET_REC_MAX: u64 = 64 << 10;

/// What a valid `set.rec` says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetRec {
    pub id: String,
    pub release: String,
    pub kernel_sha256: String,
    pub kernel_size: u64,
    /// sha256 and size; `None` only for a recovery image without one.
    pub initrd: Option<(String, u64)>,
    pub cmdline: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Bad {
    Record(record::Bad),
    /// `id` isn't the directory's name.
    Id,
    KernelSha,
    KernelSize,
    Initrd,
    /// The command line is too long, lacks one of its three keys or holds the
    /// loader's.
    Cmdline(&'static str),
}

fn is_sha(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}

fn size(s: &str) -> Option<u64> {
    if s.is_empty() || !s.bytes().all(|c| c.is_ascii_digit()) {
        return None;
    }
    s.parse().ok()
}

/// Is `id` a set id: 16 lower-case hex digits.
pub fn is_id(id: &str) -> bool {
    id.len() == 16 && id.bytes().all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}

/// Parses and checks `set.rec` of the directory `dir_name`. `recovery`: the set is a
/// recovery image, which may have no `initrd`.
pub fn parse(b: &[u8], dir_name: &str, recovery: bool) -> Result<SetRec, Bad> {
    let r = record::parse(b).map_err(Bad::Record)?;
    let id = r.get("id");
    if !is_id(id) || id != dir_name {
        return Err(Bad::Id);
    }
    if !is_sha(r.get("kernel_sha256")) {
        return Err(Bad::KernelSha);
    }
    let kernel_size = size(r.get("kernel_size")).ok_or(Bad::KernelSize)?;
    let (isha, isize) = (r.get("initrd_sha256"), r.get("initrd_size"));
    let initrd = if isha.is_empty() && isize.is_empty() {
        if !recovery {
            return Err(Bad::Initrd);
        }
        None
    } else {
        if !is_sha(isha) {
            return Err(Bad::Initrd);
        }
        Some((isha.to_string(), size(isize).ok_or(Bad::Initrd)?))
    };
    let cmdline = r.get("cmdline");
    check_cmdline(cmdline, id)?;
    Ok(SetRec {
        id: id.to_string(),
        release: r.get("release").to_string(),
        kernel_sha256: r.get("kernel_sha256").to_string(),
        kernel_size,
        initrd,
        cmdline: cmdline.to_string(),
    })
}

fn check_cmdline(c: &str, id: &str) -> Result<(), Bad> {
    if c.len() > CMDLINE_MAX {
        return Err(Bad::Cmdline("longer than 1920 bytes"));
    }
    if c.bytes().any(|b| b < 0x20 || b == 0x7f) {
        return Err(Bad::Cmdline("holds a control character"));
    }
    let words = || c.split(' ');
    if !words().any(|w| w.starts_with("glassine.system=")) {
        return Err(Bad::Cmdline("lacks glassine.system="));
    }
    if !words().any(|w| w.starts_with("glassine.userdata=")) {
        return Err(Bad::Cmdline("lacks glassine.userdata="));
    }
    if !words().any(|w| w.strip_prefix("glassine.img=") == Some(id)) {
        return Err(Bad::Cmdline("lacks glassine.img=<its id>"));
    }
    if words().any(|w| w.starts_with(DEPLOY_ONCE)) {
        return Err(Bad::Cmdline("holds glassine.deploy.once="));
    }
    Ok(())
}

/// The command line a set starts with: its own, and the one-shot deployment request
/// when one was picked (010 FR-042).
pub fn start_cmdline(set: &SetRec, deploy_once: Option<&str>) -> String {
    let mut c = set.cmdline.clone();
    if let Some(id) = deploy_once {
        if !c.is_empty() {
            c.push(' ');
        }
        c.push_str(DEPLOY_ONCE);
        c.push_str(id);
    }
    c
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sha256_hex;
    use alloc::format;

    const SET: &[u8] = include_bytes!("../../tests/golden/pc-boot/set.rec");
    const REC: &[u8] = include_bytes!("../../tests/golden/pc-boot/recovery-set.rec");

    fn with(body: &str) -> String {
        format!("{body}check={}\n", sha256_hex(body.as_bytes()))
    }

    fn body(b: &[u8]) -> String {
        let t = core::str::from_utf8(b).unwrap();
        t[..t.rfind("check=").unwrap()].to_string()
    }

    #[test]
    fn the_installers_golden_sets_check() {
        let s = parse(SET, "1abb421e678cbd3b", false).unwrap();
        assert_eq!(s.kernel_size, 12345678);
        assert_eq!(s.initrd.as_ref().unwrap().1, 34567890);
        // The loader record names it by this sum (installer golden loader.a).
        assert_eq!(sha256_hex(SET), "b8395cb8a7c1fed872d441f3482a1ba10693df711522d63b387be3fadddd7562");
        let r = parse(REC, "714c2000398993cc", true).unwrap();
        assert!(r.initrd.is_none());
        assert_eq!(sha256_hex(REC), "b73bb2df6aad60f06250a87d2509e0d7222fb4ec756f8d2587014c3e3425bad1");
    }

    #[test]
    fn a_set_rec_that_breaks_a_rule_does_not_check() {
        assert_eq!(parse(SET, "1abb421e678cbd3c", false).unwrap_err(), Bad::Id);
        assert_eq!(parse(REC, "714c2000398993cc", false).unwrap_err(), Bad::Initrd, "no initrd: recovery only");
        let mut cut = SET.to_vec();
        cut.pop();
        assert!(matches!(parse(&cut, "1abb421e678cbd3b", false), Err(Bad::Record(_))));
        let b = body(SET);
        let id = "1abb421e678cbd3b";
        let bad = |from: &str, to: &str| parse(with(&b.replace(from, to)).as_bytes(), id, false).unwrap_err();
        assert_eq!(bad("kernel_size=12345678", "kernel_size=12x"), Bad::KernelSize);
        assert_eq!(bad("kernel_size=12345678", "kernel_size="), Bad::KernelSize);
        assert_eq!(bad("kernel_sha256=6923", "kernel_sha256=6923a"), Bad::KernelSha);
        assert_eq!(bad("initrd_size=34567890", "initrd_size="), Bad::Initrd);
        assert!(matches!(bad(" glassine.img=1abb421e678cbd3b", ""), Bad::Cmdline(_)));
        assert!(matches!(bad(" glassine.img=1abb421e678cbd3b", " glassine.img=0000000000000000"), Bad::Cmdline(_)));
        assert!(matches!(bad("glassine.system=", "glassine.sys="), Bad::Cmdline(_)));
        assert!(matches!(bad("glassine.userdata=", "glassine.data="), Bad::Cmdline(_)));
        assert!(matches!(bad("quiet", "quiet glassine.deploy.once=x"), Bad::Cmdline(_)));
        let long = "a".repeat(1800);
        assert!(matches!(bad("quiet", &long), Bad::Cmdline(_)));
        assert!(matches!(bad("quiet", "quiet\tx"), Bad::Cmdline(_)));
        // At the limit is fine.
        let c = parse(SET, id, false).unwrap().cmdline;
        let pad = "a".repeat(CMDLINE_MAX - c.len() + "quiet".len());
        assert!(parse(with(&b.replace("quiet", &pad)).as_bytes(), id, false).is_ok());
    }

    #[test]
    fn only_the_deploy_request_is_added() {
        let s = parse(SET, "1abb421e678cbd3b", false).unwrap();
        assert_eq!(start_cmdline(&s, None), s.cmdline);
        assert_eq!(start_cmdline(&s, Some("d1")), format!("{} glassine.deploy.once=d1", s.cmdline));
    }
}
