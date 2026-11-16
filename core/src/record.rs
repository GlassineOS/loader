// SPDX-License-Identifier: GPL-2.0-or-later
//! The records of the boot partition (`boot-partition.md`, "Records") and the loader
//! record's two slots ("The loader record", "Slots").
//!
//! A record is text: `key=value` lines, each ended by a newline, no carriage return,
//! UTF-8, `format=1` first, no key twice, and last `check=<sha256 of every byte before
//! this line>`. The reading is the installer's (`core/src/pc_boot.rs`, `parse_record`,
//! `read_slot`, `record_in_use`), so both programs agree on every byte.
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::sha256_hex;

/// The size of each slot file.
pub const SLOT_BYTES: usize = 16384;
/// The slots, a then b.
pub const SLOTS: [&str; 2] = ["\\glassine\\loader.a", "\\glassine\\loader.b"];

/// Why a record is not valid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bad {
    NotUtf8,
    NotLines,
    NoCheckLine,
    CheckMismatch,
    NotKeyValue,
    DuplicateKey,
    NotFormat1,
    Empty,
    TrailingBytes,
    NoSeq,
}

/// A valid record: its lines in their order (without the check line).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    lines: Vec<(String, String)>,
}

impl Record {
    /// The value of `key`; a missing key is empty.
    pub fn get(&self, key: &str) -> &str {
        self.lines.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str()).unwrap_or("")
    }

    /// Whether `key` is present at all.
    pub fn has(&self, key: &str) -> bool {
        self.lines.iter().any(|(k, _)| k == key)
    }

    /// Sets `key` where it is, or adds it after the last line.
    pub fn set(&mut self, key: &str, value: &str) {
        debug_assert!(!value.contains('\n') && !key.contains('=') && !key.is_empty());
        match self.lines.iter_mut().find(|(k, _)| k == key) {
            Some((_, v)) => *v = value.to_string(),
            None => self.lines.push((key.to_string(), value.to_string())),
        }
    }

    pub fn lines(&self) -> &[(String, String)] {
        &self.lines
    }

    /// The record's text with its check line.
    pub fn text(&self) -> String {
        let mut body = String::new();
        for (k, v) in &self.lines {
            body.push_str(k);
            body.push('=');
            body.push_str(v);
            body.push('\n');
        }
        let check = sha256_hex(body.as_bytes());
        body.push_str("check=");
        body.push_str(&check);
        body.push('\n');
        body
    }
}

/// Parses one record; the whole of `b` is the record.
pub fn parse(b: &[u8]) -> Result<Record, Bad> {
    let text = core::str::from_utf8(b).map_err(|_| Bad::NotUtf8)?;
    if !text.ends_with('\n') || text.contains('\r') {
        return Err(Bad::NotLines);
    }
    let body_end = text[..text.len() - 1].rfind('\n').map(|i| i + 1).unwrap_or(0);
    let (body, last) = text.split_at(body_end);
    let check = last
        .strip_suffix('\n')
        .and_then(|l| l.strip_prefix("check="))
        .ok_or(Bad::NoCheckLine)?;
    if check != sha256_hex(body.as_bytes()) {
        return Err(Bad::CheckMismatch);
    }
    let mut lines: Vec<(String, String)> = Vec::new();
    for (i, line) in body.split_terminator('\n').enumerate() {
        let (k, v) = line.split_once('=').ok_or(Bad::NotKeyValue)?;
        if k.is_empty() {
            return Err(Bad::NotKeyValue);
        }
        if lines.iter().any(|(x, _)| x == k) {
            return Err(Bad::DuplicateKey);
        }
        if i == 0 && line != "format=1" {
            return Err(Bad::NotFormat1);
        }
        lines.push((k.to_string(), v.to_string()));
    }
    if lines.is_empty() {
        return Err(Bad::Empty);
    }
    Ok(Record { lines })
}

/// A slot's `seq` and record, if the slot is valid: a valid record, then nothing but
/// newlines, and a decimal `seq`.
pub fn read_slot(b: &[u8]) -> Result<(u64, Record), Bad> {
    let end = b.iter().rposition(|c| *c != b'\n').ok_or(Bad::Empty)? + 1;
    if end == b.len() {
        return Err(Bad::NotLines); // the check line's newline is missing
    }
    let rec = parse(&b[..end + 1]).map_err(|e| if e == Bad::NoCheckLine { Bad::TrailingBytes } else { e })?;
    let seq = rec.get("seq");
    if seq.is_empty() || !seq.bytes().all(|c| c.is_ascii_digit()) {
        return Err(Bad::NoSeq);
    }
    let n = seq.parse::<u64>().map_err(|_| Bad::NoSeq)?;
    Ok((n, rec))
}

/// The record in use and where it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InUse {
    /// 0 for slot a, 1 for slot b.
    pub slot: usize,
    pub seq: u64,
    pub rec: Record,
    /// Both slots were valid: only then is a request taken (010 FR-008).
    pub both_valid: bool,
}

/// The record in use: the valid slot with the higher `seq`, slot a on a tie. `None` for
/// a slot means the file was missing or unreadable.
pub fn in_use(a: Option<&[u8]>, b: Option<&[u8]>) -> Option<InUse> {
    let ra = a.and_then(|x| read_slot(x).ok());
    let rb = b.and_then(|x| read_slot(x).ok());
    match (ra, rb) {
        (Some((sa, ka)), Some((sb, kb))) => Some(if sb > sa {
            InUse { slot: 1, seq: sb, rec: kb, both_valid: true }
        } else {
            InUse { slot: 0, seq: sa, rec: ka, both_valid: true }
        }),
        (Some((s, k)), None) => Some(InUse { slot: 0, seq: s, rec: k, both_valid: false }),
        (None, Some((s, k))) => Some(InUse { slot: 1, seq: s, rec: k, both_valid: false }),
        (None, None) => None,
    }
}

/// Why a write was not made.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteBad {
    /// The record with its check line would not fit its slot.
    TooLarge,
    /// `seq` can't be raised.
    SeqOverflow,
}

/// The next record and the bytes of the slot it goes to: `rec` with `changes` applied
/// (every other line kept as it was, in its order), `seq` one higher, padded with
/// newlines to [`SLOT_BYTES`]. The slot is the one not in use.
pub fn next(cur: &InUse, changes: &[(&str, &str)]) -> Result<(InUse, Vec<u8>), WriteBad> {
    let seq = cur.seq.checked_add(1).ok_or(WriteBad::SeqOverflow)?;
    let mut rec = cur.rec.clone();
    let seq_s = seq.to_string();
    rec.set("seq", &seq_s);
    for (k, v) in changes {
        rec.set(k, v);
    }
    let text = rec.text();
    if text.len() > SLOT_BYTES {
        return Err(WriteBad::TooLarge);
    }
    let mut b = text.into_bytes();
    b.resize(SLOT_BYTES, b'\n');
    Ok((InUse { slot: 1 - cur.slot, seq, rec, both_valid: true }, b))
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::format;
    use alloc::vec;

    pub(crate) fn installer_record() -> String {
        // The installer's first record (installer core/tests/golden/pc-boot/loader.a).
        let body = "format=1\nseq=1\ndefault=1abb421e678cbd3b\ndefault_rec=b8395cb8a7c1fed872d441f3482a1ba10693df711522d63b387be3fadddd7562\nprevious=\nprevious_rec=\ntrial=\ntrial_rec=\nrecovery=714c2000398993cc\nrecovery_rec=b73bb2df6aad60f06250a87d2509e0d7222fb4ec756f8d2587014c3e3425bad1\nrequest=\nunmarked=0\nstarted=\nstarted_why=\ndeploys=0\n";
        format!("{body}check={}\n", sha256_hex(body.as_bytes()))
    }

    fn slot(text: &str) -> Vec<u8> {
        let mut b = text.as_bytes().to_vec();
        b.resize(SLOT_BYTES, b'\n');
        b
    }

    #[test]
    fn the_installers_golden_slot_is_read() {
        let golden = include_bytes!("../../tests/golden/pc-boot/loader.a");
        assert_eq!(golden.len(), SLOT_BYTES);
        let (seq, rec) = read_slot(golden).unwrap();
        assert_eq!(seq, 1);
        assert_eq!(rec.get("default"), "1abb421e678cbd3b");
        assert_eq!(rec.text(), installer_record());
        assert_eq!(golden.as_slice(), slot(&installer_record()).as_slice());
    }

    #[test]
    fn a_record_is_valid_only_in_its_form() {
        let good = installer_record();
        assert!(parse(good.as_bytes()).is_ok());
        let bad = |s: &str| parse(s.as_bytes()).unwrap_err();
        let with = |body: &str| format!("{body}check={}\n", sha256_hex(body.as_bytes()));
        assert_eq!(bad(&good.replace("seq=1", "seq=2")), Bad::CheckMismatch);
        assert_eq!(bad(&good[..good.len() - 1]), Bad::NotLines);
        assert_eq!(bad(&good.replace('\n', "\r\n")), Bad::NotLines);
        assert_eq!(bad(&with("format=2\nseq=1\n")), Bad::NotFormat1);
        assert_eq!(bad(&with("seq=1\nformat=1\n")), Bad::NotFormat1);
        assert_eq!(bad(&with("format=1\nseq=1\nseq=2\n")), Bad::DuplicateKey);
        assert_eq!(bad(&with("format=1\nnoequals\n")), Bad::NotKeyValue);
        assert_eq!(bad(&with("format=1\n=x\n")), Bad::NotKeyValue);
        assert_eq!(bad(&with("format=1\n\n")), Bad::NotKeyValue);
        assert_eq!(bad(&with("")), Bad::Empty);
        assert_eq!(bad("format=1\n"), Bad::NoCheckLine);
        assert_eq!(bad(&good.replace(&good[good.len() - 65..good.len() - 1], &good[good.len() - 65..good.len() - 1].to_uppercase())), Bad::CheckMismatch);
        assert_eq!(parse(&[0xff, b'\n']).unwrap_err(), Bad::NotUtf8);
        // Unknown keys are kept, a value may hold '=' and spaces.
        let r = parse(with("format=1\nseq=4\nfuture=a=b c\n").as_bytes()).unwrap();
        assert_eq!(r.get("future"), "a=b c");
        assert_eq!(r.get("missing"), "");
    }

    #[test]
    fn a_slot_is_valid_only_with_newlines_after_its_check() {
        let good = slot(&installer_record());
        assert!(read_slot(&good).is_ok());
        let mut b = good.clone();
        b[SLOT_BYTES - 1] = b'x';
        assert!(read_slot(&b).is_err());
        let mut b = good.clone();
        b[SLOT_BYTES - 1] = 0;
        assert!(read_slot(&b).is_err());
        assert!(read_slot(&vec![b'\n'; SLOT_BYTES]).is_err());
        assert!(read_slot(&vec![0; SLOT_BYTES]).is_err());
        assert!(read_slot(&[]).is_err());
        let text = installer_record();
        assert!(read_slot(&text.as_bytes()[..text.len() - 1]).is_err(), "no newline after the check");
        let nos = installer_record().replace("seq=1\n", "seq=\n");
        let body = &nos[..nos.rfind("check=").unwrap()];
        let nos = format!("{body}check={}\n", sha256_hex(body.as_bytes()));
        assert_eq!(read_slot(&slot(&nos)).unwrap_err(), Bad::NoSeq);
    }

    fn with_seq(seq: u64) -> Vec<u8> {
        let text = installer_record();
        let body = &text[..text.rfind("check=").unwrap()];
        let body = body.replace("seq=1\n", &format!("seq={seq}\n"));
        slot(&format!("{body}check={}\n", sha256_hex(body.as_bytes())))
    }

    #[test]
    fn the_record_in_use_is_the_higher_seq_a_on_a_tie() {
        let (a, b) = (with_seq(4), with_seq(5));
        assert_eq!(in_use(Some(&a), Some(&b)).unwrap().slot, 1);
        assert_eq!(in_use(Some(&b), Some(&a)).unwrap().slot, 0);
        let t = in_use(Some(&a), Some(&a)).unwrap();
        assert!(t.slot == 0 && t.both_valid);
        let one = in_use(Some(&a), Some(&[1, 2, 3])).unwrap();
        assert!(one.slot == 0 && !one.both_valid);
        let one = in_use(None, Some(&b)).unwrap();
        assert!(one.slot == 1 && !one.both_valid && one.seq == 5);
        assert!(in_use(None, Some(b"junk")).is_none());
        assert!(in_use(None, None).is_none());
    }

    #[test]
    fn a_write_keeps_other_keys_and_goes_to_the_other_slot() {
        let text = installer_record();
        let body = &text[..text.rfind("check=").unwrap()];
        let body = format!("{body}zz_unknown=keep me\n");
        let a = slot(&format!("{body}check={}\n", sha256_hex(body.as_bytes())));
        let cur = in_use(Some(&a), Some(&a)).unwrap();
        let (n, bytes) = next(&cur, &[("unmarked", "1"), ("started", "1abb421e678cbd3b"), ("started_why", "default")]).unwrap();
        assert_eq!(n.slot, 1);
        assert_eq!(bytes.len(), SLOT_BYTES);
        let (seq, rec) = read_slot(&bytes).unwrap();
        assert_eq!(seq, 2);
        assert_eq!(rec.get("zz_unknown"), "keep me");
        assert_eq!(rec.get("unmarked"), "1");
        // The order of the lines is the order read.
        let keys: Vec<&str> = rec.lines().iter().map(|(k, _)| k.as_str()).collect();
        let old: Vec<&str> = cur.rec.lines().iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(keys, old);
        // Written slot b is now in use; the next write goes to a.
        let both = in_use(Some(&a), Some(&bytes)).unwrap();
        assert_eq!(both.slot, 1);
        assert_eq!(next(&both, &[]).unwrap().0.slot, 0);
    }

    #[test]
    fn a_record_that_would_not_fit_is_not_written() {
        let a = slot(&installer_record());
        let cur = in_use(Some(&a), None).unwrap();
        let big = "x".repeat(SLOT_BYTES);
        assert_eq!(next(&cur, &[("big", &big)]).unwrap_err(), WriteBad::TooLarge);
    }

    /// A power cut at every byte of the write: the slot being written holds the first
    /// `k` bytes of the new slot and the rest of what it held. The record read is the
    /// old one, until the write is whole. Checked over an old slot holding a valid older
    /// record, an invalid one, zeros, and nothing (a file of size `k`).
    #[test]
    fn a_cut_at_every_byte_leaves_the_old_or_the_new_record() {
        let a = with_seq(7);
        let b_older = with_seq(6);
        let cur = in_use(Some(&a), Some(&b_older)).unwrap();
        assert_eq!(cur.slot, 0);
        let (_, new_b) = next(&cur, &[("unmarked", "2"), ("request", "")]).unwrap();
        let new_rec = read_slot(&new_b).unwrap().1;
        let new_len = new_rec.text().len();
        let mut garbage = b_older.clone();
        garbage[100] ^= 1;
        let olds: [Option<Vec<u8>>; 4] = [Some(b_older.clone()), Some(garbage), Some(vec![0; SLOT_BYTES]), None];
        for old in &olds {
            for k in 0..=SLOT_BYTES {
                let mut cut = new_b[..k].to_vec();
                if let Some(o) = old {
                    cut.extend_from_slice(&o[k..]);
                }
                let got = in_use(Some(&a), Some(&cut)).unwrap();
                if k == SLOT_BYTES {
                    assert_eq!((got.slot, &got.rec), (1, &new_rec), "whole write");
                } else if got.slot == 1 {
                    // Only the pad newlines were missing: the record is the new one.
                    assert_eq!(got.rec, new_rec, "cut at {k}: slot b is in use but not new");
                    // (at new_len - 1 the old slot may supply the final newline)
                    assert!(k + 1 >= new_len, "cut at {k}: before the record's end ({new_len})");
                } else {
                    assert_eq!(got.rec, cur.rec, "cut at {k}");
                }
            }
        }
    }
}
