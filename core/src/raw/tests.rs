// SPDX-License-Identifier: GPL-2.0-or-later
//! The hand-back partition on a fake block device: the raw record, a write cut at every
//! byte, the answers of each record state, and the loader image header.
use super::*;
use crate::answer::{self, Boot};
use crate::plan::{self, Log};
use crate::record::{self, InUse};
use crate::sha256_hex;
use alloc::format;
use alloc::string::String;
use std::string::ToString;

const BS: usize = 512;
const SIZE: usize = 4 << 20;

#[derive(Clone)]
struct Mem {
    bytes: Vec<u8>,
    read_only: bool,
    /// The next write lands only this many bytes, then fails (a power cut).
    cut: Option<usize>,
    /// Every later read of a written slot returns one bit wrong.
    corrupt_readback: bool,
    writes: usize,
    flushes: usize,
}

impl Mem {
    fn new() -> Mem {
        Mem { bytes: vec![0; SIZE], read_only: false, cut: None, corrupt_readback: false, writes: 0, flushes: 0 }
    }
}

impl Blocks for Mem {
    fn block_size(&self) -> u32 {
        BS as u32
    }
    fn blocks(&self) -> u64 {
        (self.bytes.len() / BS) as u64
    }
    fn read_only(&self) -> bool {
        self.read_only
    }
    fn read(&mut self, lba: u64, buf: &mut [u8]) -> bool {
        let o = lba as usize * BS;
        if buf.len() % BS != 0 || o + buf.len() > self.bytes.len() {
            return false;
        }
        buf.copy_from_slice(&self.bytes[o..o + buf.len()]);
        if self.corrupt_readback && self.writes > 0 {
            buf[0] ^= 1;
        }
        true
    }
    fn write(&mut self, lba: u64, buf: &[u8]) -> bool {
        let o = lba as usize * BS;
        if buf.len() % BS != 0 || o + buf.len() > self.bytes.len() {
            return false;
        }
        self.writes += 1;
        let n = self.cut.take().map_or(buf.len(), |c| c.min(buf.len()));
        self.bytes[o..o + n].copy_from_slice(&buf[..n]);
        n == buf.len()
    }
    fn flush(&mut self) -> bool {
        self.flushes += 1;
        true
    }
}

struct Disk {
    raw: Raw<Mem>,
    log: Vec<String>,
}

impl plan::RecordStore for Disk {
    fn read_slot(&mut self, slot: usize) -> Result<Vec<u8>, plan::ReadErr> {
        self.raw.read_slot(slot)
    }
    fn write_slot(&mut self, slot: usize, bytes: &[u8]) -> bool {
        self.raw.write_slot(slot, bytes)
    }
}

impl Log for Disk {
    fn log(&mut self, line: core::fmt::Arguments) {
        self.log.push(format!("{line}"));
    }
}

fn disk(m: Mem) -> Disk {
    Disk { raw: Raw::new(m).unwrap(), log: Vec::new() }
}

fn checked(body: &str) -> String {
    format!("{body}check={}\n", sha256_hex(body.as_bytes()))
}

const SET1: &str = "0123456789abcdef";
const SET2: &str = "fedcba9876543210";

fn slot(seq: u64, keys: &[(&str, &str)]) -> Vec<u8> {
    let mut body = format!("format=1\nseq={seq}\n");
    let base = [
        ("default", ""),
        ("default_rec", ""),
        ("previous", ""),
        ("previous_rec", ""),
        ("trial", ""),
        ("trial_rec", ""),
        ("recovery", ""),
        ("recovery_rec", ""),
        ("request", ""),
        ("unmarked", "0"),
        ("started", ""),
        ("started_why", ""),
        ("deploys", "0"),
    ];
    for (k, v) in base {
        let v = keys.iter().find(|(kk, _)| *kk == k).map(|(_, v)| *v).unwrap_or(v);
        body.push_str(&format!("{k}={v}\n"));
    }
    for (k, v) in keys {
        if !base.iter().any(|(b, _)| b == k) {
            body.push_str(&format!("{k}={v}\n"));
        }
    }
    let mut b = checked(&body).into_bytes();
    b.resize(record::SLOT_BYTES, b'\n');
    b
}

/// A partition with the same record in both slots, as the installer writes it.
fn both(keys: &[(&str, &str)]) -> Mem {
    let mut m = Mem::new();
    let s = slot(1, keys);
    m.bytes[0..16384].copy_from_slice(&s);
    m.bytes[16384..32768].copy_from_slice(&s);
    m
}

fn answer(d: &mut Disk) -> (answer::Answer, Option<InUse>) {
    let r = plan::read_record(d);
    let a = plan::handback(d, r, false);
    let after = plan::read_record(d);
    (a, after)
}

fn keys(r: &Option<InUse>) -> (u64, String, String, String, String) {
    let r = r.as_ref().unwrap();
    (r.seq, r.rec.get("unmarked").into(), r.rec.get("started").into(), r.rec.get("started_why").into(), r.rec.get("request").into())
}

// ---- the partition -----------------------------------------------------------------------

#[test]
fn a_block_size_that_doesnt_divide_4096_or_a_small_partition_is_refused() {
    struct Odd(u32, u64);
    impl Blocks for Odd {
        fn block_size(&self) -> u32 {
            self.0
        }
        fn blocks(&self) -> u64 {
            self.1
        }
        fn read_only(&self) -> bool {
            false
        }
        fn read(&mut self, _: u64, _: &mut [u8]) -> bool {
            false
        }
        fn write(&mut self, _: u64, _: &[u8]) -> bool {
            false
        }
        fn flush(&mut self) -> bool {
            false
        }
    }
    assert_eq!(Raw::new(Odd(520, 1 << 20)).err(), Some(Unusable::BlockSize(520)));
    assert_eq!(Raw::new(Odd(0, 1 << 20)).err(), Some(Unusable::BlockSize(0)));
    assert_eq!(Raw::new(Odd(4096, 1023)).err(), Some(Unusable::TooSmall(1023 * 4096)));
    assert!(Raw::new(Odd(4096, 1024)).is_ok());
    assert!(Raw::new(Odd(512, 8192)).is_ok());
}

#[test]
fn a_partition_never_written_has_no_record_and_answers_the_baseline_writing_nothing() {
    let mut d = disk(Mem::new());
    let (a, after) = answer(&mut d);
    assert_eq!((a.boot, a.why), (Boot::Baseline, "no-record"));
    assert!(after.is_none());
    assert_eq!(d.raw.dev.writes, 0);
}

// ---- the answers --------------------------------------------------------------------------

#[test]
fn an_ordinary_start_counts_and_answers_the_baseline() {
    let mut d = disk(both(&[]));
    let (a, after) = answer(&mut d);
    assert_eq!((a.boot, a.why), (Boot::Baseline, "default"));
    assert_eq!(keys(&after), (2, "1".into(), "".into(), "default".into(), "".into()));
    assert_eq!(after.unwrap().slot, 1, "the slot not in use was written");
    assert_eq!(d.raw.dev.flushes, 1);
    assert_eq!(&d.raw.dev.bytes[0..16384], &slot(1, &[])[..], "slot a untouched");
}

#[test]
fn a_named_default_set_is_answered_by_id() {
    let rec = "ab".repeat(32);
    let mut d = disk(both(&[("default", SET1), ("default_rec", &rec)]));
    let (a, after) = answer(&mut d);
    assert_eq!(a.boot, Boot::Set { id: SET1.into(), rec });
    assert_eq!(keys(&after), (2, "1".into(), SET1.into(), "default".into(), "".into()));
}

#[test]
fn at_the_limit_the_previous_set_then_the_recovery_uncounted() {
    let mut d = disk(both(&[("unmarked", "3"), ("previous", SET2)]));
    let (a, after) = answer(&mut d);
    assert_eq!((a.boot, a.why), (Boot::Set { id: SET2.into(), rec: "".into() }, "limit"));
    assert_eq!(keys(&after), (2, "4".into(), SET2.into(), "limit".into(), "".into()));
    let w = d.raw.dev.writes;
    let (a, after) = answer(&mut d);
    assert_eq!((a.boot, a.why), (Boot::Recovery, "limit"));
    assert_eq!(after.unwrap().seq, 2, "a start of the recovery is not counted");
    assert_eq!(d.raw.dev.writes, w);
}

#[test]
fn at_the_limit_with_no_previous_set_the_recovery() {
    let mut d = disk(both(&[("unmarked", "3")]));
    let (a, _) = answer(&mut d);
    assert_eq!((a.boot, a.why), (Boot::Recovery, "limit"));
    assert_eq!(d.raw.dev.writes, 0);
}

#[test]
fn a_recovery_request_is_cleared_before_the_answer_and_not_counted() {
    let mut d = disk(both(&[("request", "recovery"), ("unmarked", "1")]));
    let (a, after) = answer(&mut d);
    assert_eq!((a.boot, a.why), (Boot::Recovery, "request"));
    assert_eq!(keys(&after), (2, "1".into(), "".into(), "".into(), "".into()));
    // The next start is the ordinary one.
    let (a, _) = answer(&mut d);
    assert_eq!((a.boot, a.why), (Boot::Baseline, "default"));
}

#[test]
fn a_trial_request_is_cleared_and_the_trial_set_answered_uncounted() {
    let mut d = disk(both(&[("request", "trial"), ("trial", SET2), ("trial_rec", "cd")]));
    let (a, after) = answer(&mut d);
    assert_eq!((a.boot, a.why), (Boot::Set { id: SET2.into(), rec: "cd".into() }, "request"));
    assert_eq!(keys(&after), (2, "0".into(), SET2.into(), "request".into(), "".into()));
}

#[test]
fn a_trial_request_without_a_trial_set_goes_by_the_count() {
    let mut d = disk(both(&[("request", "trial")]));
    let (a, after) = answer(&mut d);
    assert_eq!((a.boot, a.why), (Boot::Baseline, "default"));
    assert_eq!(keys(&after), (3, "1".into(), "".into(), "default".into(), "".into()));
}

#[test]
fn with_one_valid_slot_no_request_is_taken_and_the_write_drops_it() {
    let mut m = both(&[("request", "recovery")]);
    m.bytes[16384..32768].fill(0);
    let mut d = disk(m);
    let (a, after) = answer(&mut d);
    assert_eq!((a.boot, a.why), (Boot::Baseline, "default"));
    assert_eq!(keys(&after), (2, "1".into(), "".into(), "default".into(), "".into()));
}

#[test]
fn a_record_that_cant_be_written_answers_the_baseline_uncounted_and_keeps_the_request() {
    for ro in [true, false] {
        let mut m = both(&[("request", "recovery")]);
        m.read_only = ro;
        m.corrupt_readback = !ro;
        let mut d = disk(m);
        let (a, _) = answer(&mut d);
        assert_eq!((a.boot, a.why), (Boot::Baseline, "not-written"), "read-only {ro}");
    }
    let mut m = both(&[("unmarked", "3"), ("previous", SET2)]);
    m.read_only = true;
    let mut d = disk(m);
    let (a, after) = answer(&mut d);
    assert_eq!((a.boot, a.why), (Boot::Baseline, "not-written"));
    assert_eq!(after.unwrap().seq, 1);
}

#[test]
fn the_persons_recovery_clears_only_a_recovery_request() {
    let mut d = disk(both(&[("request", "trial"), ("trial", SET2)]));
    let r = plan::read_record(&mut d);
    let a = plan::handback(&mut d, r, true);
    assert_eq!((a.boot, a.why), (Boot::Recovery, "choice"));
    assert_eq!(d.raw.dev.writes, 0, "a trial request is left for the next start");
    let mut d = disk(both(&[("request", "recovery")]));
    let r = plan::read_record(&mut d);
    plan::handback(&mut d, r, true);
    assert_eq!(keys(&plan::read_record(&mut d)).4, "");
}

#[test]
fn unknown_keys_and_the_probe_key_are_kept() {
    let mut d = disk(both(&[("probe", "1"), ("future", "x y")]));
    let (_, after) = answer(&mut d);
    let r = after.unwrap();
    assert_eq!((r.rec.get("probe"), r.rec.get("future")), ("1", "x y"));
}

// ---- a cut write ------------------------------------------------------------------------

#[test]
fn a_counting_write_cut_at_every_byte_reads_the_old_or_the_new_record() {
    let start = both(&[("default", SET1), ("unmarked", "1")]);
    let old = record::in_use(Some(&start.bytes[0..16384]), Some(&start.bytes[16384..32768])).unwrap();
    let mut full = disk(start.clone());
    answer(&mut full);
    let new = plan::read_record(&mut full).unwrap();
    assert_eq!(new.seq, 2);
    let mut seen_new = 0;
    for cut in 0..=record::SLOT_BYTES {
        let mut m = start.clone();
        m.cut = Some(cut);
        let mut d = disk(m);
        let r = plan::read_record(&mut d);
        let a = plan::handback(&mut d, r, false);
        // Read what a power cut leaves, from the bytes alone.
        let b = &d.raw.dev.bytes;
        let got = record::in_use(Some(&b[0..16384]), Some(&b[16384..32768])).unwrap();
        assert_eq!(&b[0..16384], &start.bytes[0..16384], "cut {cut}: the slot in use was written");
        if got == new {
            seen_new += 1;
        } else {
            assert_eq!(got.rec, old.rec, "cut {cut}: neither the old nor the new record");
            assert_eq!(got.seq, old.seq);
            if cut < record::SLOT_BYTES {
                assert_eq!(a.why, "not-written", "cut {cut}: a cut write counts nothing");
            }
        }
    }
    assert!(seen_new >= 1, "the whole write gives the new record");
}

#[test]
fn a_request_write_cut_at_every_byte_never_takes_the_request_twice_or_loses_the_old_record() {
    let start = both(&[("request", "recovery")]);
    for cut in (0..record::SLOT_BYTES).step_by(7).chain([record::SLOT_BYTES - 1]) {
        let mut m = start.clone();
        m.cut = Some(cut);
        let mut d = disk(m);
        let r = plan::read_record(&mut d);
        let a = plan::handback(&mut d, r, false);
        assert_eq!(a.boot, Boot::Baseline, "cut {cut}: the recovery only after a durable clear");
        // The next start reads the old record (one valid slot: the request is not
        // taken) or, when the cut fell in the padding of a slot that already held
        // newlines there, the new one with the request cleared.
        let r = plan::read_record(&mut d).unwrap();
        match r.seq {
            1 => assert_eq!(r.rec.get("request"), "recovery", "cut {cut}"),
            2 => assert_eq!(r.rec.get("request"), "", "cut {cut}"),
            n => panic!("cut {cut}: seq {n}"),
        }
        if !r.both_valid {
            let a = plan::handback(&mut d, Some(r), false);
            assert_eq!(a.boot, Boot::Baseline, "cut {cut}");
        }
    }
}

// ---- the probe area ------------------------------------------------------------------

#[test]
fn the_probe_area_is_written_alone_and_read_back() {
    let mut r = Raw::new(both(&[])).unwrap();
    let before = r.dev.bytes.clone();
    let mut p = b"format=1\n".to_vec();
    p.resize(PROBE_BYTES, b'\n');
    assert!(r.write_probe(&p));
    assert_eq!(r.read_probe().unwrap(), p);
    assert_eq!(&r.dev.bytes[..32768], &before[..32768], "the slots untouched");
    assert_eq!(&r.dev.bytes[98304..], &before[98304..], "nothing past the probe area");
    assert!(!r.write_probe(b"short"), "only a whole area");
    r.dev.read_only = true;
    assert!(!r.write_probe(&p));
}

// ---- the loader image header ----------------------------------------------------------

#[test]
fn the_header_reads_back_and_every_damage_is_refused() {
    let img = b"MZ not really a loader".to_vec();
    let h = loader_header(&img);
    assert_eq!(h.len(), 4096);
    assert_eq!(check_header(&h, SIZE as u64), Ok((img.len() as u64, crc32(&img))));
    for i in 0..32 {
        let mut b = h.clone();
        b[i] ^= 0x10;
        assert!(check_header(&b, SIZE as u64).is_err(), "byte {i}");
    }
    assert_eq!(check_header(&h[..31], SIZE as u64), Err(BadHeader::Short));
    assert_eq!(check_header(&vec![0u8; 4096], SIZE as u64), Err(BadHeader::Magic));
    // A size past the limit or past the partition, with a good header CRC.
    let mut big = vec![0u8; LOADER_MAX as usize + 1];
    big[0] = 1;
    assert_eq!(check_header(&loader_header(&big), 64 << 20), Err(BadHeader::Size));
    assert_eq!(check_header(&loader_header(&img), LOADER_IMAGE_OFFSET + 5), Err(BadHeader::Size));
    assert_eq!(check_header(&loader_header(&[]), SIZE as u64), Err(BadHeader::Size));
    let _ = "".to_string();
}
