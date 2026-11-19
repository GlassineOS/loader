// SPDX-License-Identifier: GPL-2.0-or-later
//! The boot partition's raw form on a fake block device: the region table and its
//! damage, the record in its region with a write cut at every byte, the answers of each
//! record state, a region write cut at every byte, the splash store and the override's
//! removal, and the loader image header.
use super::*;
use crate::answer::{self, Boot};
use crate::plan::{self, Image, Log, Pick, SplashStore};
use crate::record::{self, InUse};
use crate::sha256_hex;
use alloc::format;
use alloc::string::String;
use std::string::ToString;

const BS: usize = 512;
const SIZE: usize = 16 << 20;
/// The record's slots in the 16 MiB layout.
const SA: core::ops::Range<usize> = 65536..81920;
const SB: core::ops::Range<usize> = 81920..98304;

#[derive(Clone)]
struct Mem {
    bytes: Vec<u8>,
    read_only: bool,
    /// The power fails after this many more bytes are written: the write that reaches
    /// it lands only in part, and every later write lands nothing.
    cut: Option<usize>,
    /// Every later read of a written slot returns one bit wrong.
    corrupt_readback: bool,
    writes: usize,
    flushes: usize,
}

impl Mem {
    /// A partition never written: no table.
    fn blank() -> Mem {
        Mem { bytes: vec![0; SIZE], read_only: false, cut: None, corrupt_readback: false, writes: 0, flushes: 0 }
    }

    /// The installer's 16 MiB table, both copies, and nothing else.
    fn new() -> Mem {
        let mut m = Mem::blank();
        let t = build_table(SIZE as u64, &layout_16mib());
        m.bytes[..4096].copy_from_slice(&t);
        m.bytes[SIZE - 4096..].copy_from_slice(&t);
        m
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
        let n = match self.cut.as_mut() {
            None => buf.len(),
            Some(left) => {
                let n = (*left).min(buf.len());
                *left -= n;
                if n < buf.len() {
                    *left = 0;
                }
                n
            }
        };
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

impl SplashStore for Disk {
    fn read_image(&mut self, which: Image, max: u64, deadline: Option<u64>) -> Result<Vec<u8>, plan::ReadErr> {
        self.raw.read_image(which, max, deadline)
    }
    fn has_override(&mut self) -> bool {
        self.raw.has_override()
    }
    fn remove_override(&mut self) -> bool {
        self.raw.remove_override()
    }
    fn now_ms(&mut self) -> u64 {
        0
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
    m.bytes[SA].copy_from_slice(&s);
    m.bytes[SB].copy_from_slice(&s);
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
    assert_eq!(Raw::new(Odd(4096, 1)).err(), Some(Unusable::TooSmall(4096)));
    let r = Raw::new(Odd(4096, 1024)).unwrap();
    assert!(r.regions.is_none(), "a partition that can't be read holds nothing");
    assert!(Raw::new(Odd(512, 8192)).is_ok());
}

#[test]
fn a_partition_never_written_has_no_record_and_answers_the_baseline_writing_nothing() {
    for m in [Mem::blank(), Mem::new()] {
        let mut d = disk(m);
        let (a, after) = answer(&mut d);
        assert_eq!((a.boot, a.why), (Boot::Baseline, "no-record"));
        assert!(after.is_none());
        assert_eq!(d.raw.dev.writes, 0);
    }
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
    assert_eq!(&d.raw.dev.bytes[SA], &slot(1, &[])[..], "slot a untouched");
}

#[test]
fn a_deploy_once_left_by_an_earlier_pick_is_cleared_by_a_start_without_the_menu() {
    // A pick's write set it; that start never reached the initramfs (a reset, a power
    // cut): this start's count write clears it, the answer is the ordinary one.
    let mut d = disk(both(&[("deploy_once", "20261116-120000"), ("parent_cmdline", "no")]));
    let (a, after) = answer(&mut d);
    assert_eq!((a.boot, a.why, a.cmdline_add), (Boot::Baseline, "default", None));
    let after = after.unwrap();
    assert_eq!((after.rec.get("deploy_once"), after.rec.get("unmarked")), ("", "1"));
    // a record without one gets no deploy_once line
    let mut d = disk(both(&[]));
    let (_, after) = answer(&mut d);
    assert!(!after.unwrap().rec.has("deploy_once"));
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
    m.bytes[SB].fill(0);
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
    let old = record::in_use(Some(&start.bytes[SA]), Some(&start.bytes[SB])).unwrap();
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
        let got = record::in_use(Some(&b[SA]), Some(&b[SB])).unwrap();
        assert_eq!(&b[SA], &start.bytes[SA], "cut {cut}: the slot in use was written");
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

// ---- the region table ---------------------------------------------------------------

const K: u64 = 1024;
const M: u64 = 1 << 20;

#[test]
fn the_installers_table_reads_back_with_every_region() {
    let r = Raw::new(Mem::new()).unwrap();
    let (copy, regs) = r.regions.clone().unwrap();
    assert_eq!(copy, TableCopy::Primary);
    assert_eq!(regs.len(), 6);
    assert_eq!(r.region(RECORD), Some((64 * K, 32 * K)));
    assert_eq!(r.region(LOADER), Some((M, 3 * M)));
    assert_eq!(r.region("unknown"), None);
}

/// The table both the ABL's C test (GlassineOS/abl `glassine/test/handback-test.c`,
/// `SharedTable`) and this one check: the 16 MiB layout's CRC and its loader entry.
#[test]
fn the_shared_table_vector() {
    let t = build_table(16 << 20, &layout_16mib());
    assert_eq!(crc32(&t[..4092]), u32::from_le_bytes(t[4092..].try_into().unwrap()));
    assert_eq!(std::format!("{:08x}", crc32(&t)), SHARED_TABLE_CRC);
}
pub(crate) const SHARED_TABLE_CRC: &str = "2144df1c";

#[test]
fn a_flipped_byte_in_the_primary_falls_back_to_the_backup_and_in_both_to_nothing() {
    let good = Mem::new();
    for i in 0..4096 {
        let mut m = good.clone();
        m.bytes[i] ^= 0x01;
        let r = Raw::new(m.clone()).unwrap();
        let (copy, regs) = r.regions.clone().unwrap_or_else(|| panic!("byte {i}: the backup wasn't used"));
        assert_eq!(copy, TableCopy::Backup, "byte {i}");
        assert_eq!(regs.len(), 6);
        m.bytes[SIZE - 4096 + i] ^= 0x01;
        assert!(Raw::new(m).unwrap().regions.is_none(), "byte {i}: both copies damaged");
    }
}

#[test]
fn a_table_with_one_bad_entry_is_refused() {
    let part = SIZE as u64;
    let good = layout_16mib();
    let bad: [(&str, std::vec::Vec<(&str, u64, u64)>); 9] = [
        ("overlap", std::vec![(RECORD, 64 * K, 32 * K), (PROBE, 80 * K, 64 * K)]),
        ("repeated name", std::vec![(RECORD, 64 * K, 32 * K), (RECORD, 128 * K, 32 * K)]),
        ("name with a capital", std::vec![("Record", 64 * K, 32 * K)]),
        ("empty name", std::vec![("", 64 * K, 32 * K)]),
        ("offset not 4096-aligned", std::vec![(RECORD, 64 * K + 512, 32 * K)]),
        ("size not 4096-aligned", std::vec![(RECORD, 64 * K, 32 * K + 512)]),
        ("size zero", std::vec![(RECORD, 64 * K, 0)]),
        ("over the primary", std::vec![(RECORD, 0, 32 * K)]),
        ("into the backup", std::vec![(RECORD, part - 32 * K, 32 * K)]),
    ];
    assert!(parse_table(&build_table(part, &good), part).is_ok());
    for (why, regs) in bad {
        assert!(parse_table(&build_table(part, &regs), part).is_err(), "{why}");
    }
    assert_eq!(parse_table(&build_table(part, &good), part - 4096), Err(BadTable::Size), "another partition's table");
    let none: [(&str, u64, u64); 0] = [];
    assert_eq!(parse_table(&build_table(part, &none), part), Err(BadTable::Count));
    let mut t = build_table(part, &good);
    t[64 + 64 * 6] = 1; // past the last entry
    let c = crc32(&t[..4092]);
    t[4092..].copy_from_slice(&c.to_le_bytes());
    assert_eq!(parse_table(&t, part), Err(BadTable::NotZero));
    let mut t = build_table(part, &good);
    t[64 + 3] ^= 1; // an entry's name, table CRC made good again
    let c = crc32(&t[..4092]);
    t[4092..].copy_from_slice(&c.to_le_bytes());
    assert_eq!(parse_table(&t, part), Err(BadTable::Entry(0)), "the entry's own CRC");
}

#[test]
fn a_region_with_a_flag_or_a_name_it_doesnt_know_is_ignored() {
    let mut m = Mem::new();
    let mut t = build_table(SIZE as u64, &[(RECORD, 64 * K, 32 * K), ("future", 13 * M, 64 * K)]);
    t[64 + 48] = 1; // the record's flags
    let e = crc32(&t[64..64 + 52]);
    t[64 + 52..64 + 56].copy_from_slice(&e.to_le_bytes());
    let c = crc32(&t[..4092]);
    t[4092..].copy_from_slice(&c.to_le_bytes());
    m.bytes[..4096].copy_from_slice(&t);
    let r = Raw::new(m).unwrap();
    assert_eq!(r.region(RECORD), None);
    assert!(r.regions.is_some());
}

// ---- a region write --------------------------------------------------------------------

fn picture(w: u32, h: u32, fill: u8) -> Vec<u8> {
    let px = vec![fill; (w * h * 4) as usize];
    gspl::encode(0, w, h, (w, h), 0, 0, &px)
}

#[test]
fn a_region_write_cut_at_every_byte_leaves_the_old_image_none_or_the_new() {
    let old = picture(40, 30, 0x11); // 4864 bytes: a head and a tail
    let new = picture(40, 30, 0x22);
    let mut start = Mem::new();
    {
        let mut r = Raw::new(start.clone()).unwrap();
        assert!(r.write_image(Image::Splash, &old));
        start = r.dev;
    }
    // The whole write: zeroed head, the tail, the head.
    let total = 4096 + (new.len() - 4096).div_ceil(BS) * BS + 4096;
    let mut seen = [0usize; 3];
    for cut in 0..=total {
        let mut m = start.clone();
        m.cut = Some(cut);
        let mut r = Raw::new(m).unwrap();
        let ok = r.write_image(Image::Splash, &new);
        r.dev.cut = None;
        let got = r.read_image(Image::Splash, gspl::MAX_FILE, None);
        match got {
            Ok(b) if b == old => seen[0] += 1,
            Ok(b) if b == new => seen[2] += 1,
            Err(plan::ReadErr::Missing) => seen[1] += 1,
            Ok(b) => assert!(gspl::parse(&b).is_err(), "cut {cut}: a half image that parses"),
            Err(e) => panic!("cut {cut}: {e:?}"),
        }
        if cut < total {
            assert!(!ok, "cut {cut}: a cut write reported done");
        }
        // Never a mix: an image that parses is the old or the new one.
        if let Ok(b) = r.read_image(Image::Splash, gspl::MAX_FILE, None) {
            if gspl::parse(&b).is_ok() {
                assert!(b == old || b == new, "cut {cut}");
            }
        }
    }
    assert!(seen[0] >= 1 && seen[1] >= 1 && seen[2] >= 1, "{seen:?}");
}

#[test]
fn an_image_larger_than_its_region_is_refused_and_nothing_is_written() {
    let mut r = Raw::new(Mem::new()).unwrap();
    let before = r.dev.bytes.clone();
    let big = vec![1u8; (3 * M + 1) as usize];
    assert!(!r.write_image(Image::Override, &big));
    assert_eq!(r.dev.bytes, before);
    assert_eq!(r.dev.writes, 0);
}

#[test]
fn a_region_write_touches_nothing_outside_its_region() {
    let mut r = Raw::new(both(&[])).unwrap();
    let before = r.dev.bytes.clone();
    let img = picture(64, 64, 0x33);
    assert!(r.write_image(Image::Splash, &img));
    let (off, size) = r.region(SPLASH).unwrap();
    let (off, size) = (off as usize, size as usize);
    assert_eq!(&r.dev.bytes[..off], &before[..off]);
    assert_eq!(&r.dev.bytes[off + size..], &before[off + size..]);
}

// ---- the splash store, raw ---------------------------------------------------------------

#[test]
fn the_override_is_removed_before_it_is_drawn_and_drawn_once() {
    let mut m = both(&[]);
    {
        let mut r = Raw::new(m).unwrap();
        assert!(r.write_image(Image::Override, &picture(8, 6, 0x44)));
        assert!(r.write_image(Image::Splash, &picture(8, 6, 0x55)));
        m = r.dev;
    }
    let mut d = disk(m);
    assert!(d.raw.has_override());
    match plan::pick_picture(&mut d, &[(8, 6)]) {
        Pick::Override(p) => assert_eq!(p.mode, (8, 6)),
        p => panic!("{p:?}"),
    }
    assert!(d.log.iter().any(|l| l == "OVERRIDE_REMOVED"));
    assert!(!d.raw.has_override(), "removed");
    let (off, _) = d.raw.region(OVERRIDE).unwrap();
    assert!(d.raw.dev.bytes[off as usize..off as usize + 4096].iter().all(|c| *c == 0));
    match plan::pick_picture(&mut d, &[(8, 6)]) {
        Pick::Splash(_, "splash") => {}
        p => panic!("the second start: {p:?}"),
    }
}

#[test]
fn an_override_that_cant_be_removed_is_not_drawn() {
    let mut m = both(&[]);
    {
        let mut r = Raw::new(m).unwrap();
        assert!(r.write_image(Image::Override, &picture(8, 6, 0x44)));
        m = r.dev;
    }
    m.read_only = true;
    let mut d = disk(m);
    match plan::pick_picture(&mut d, &[(8, 6)]) {
        Pick::Nothing => {}
        p => panic!("{p:?}"),
    }
    assert!(d.log.iter().any(|l| l == "OVERRIDE_NOT_REMOVED: not drawn"));
    // A cut during the removal: shown at that start or never.
    let mut m = both(&[]);
    {
        let mut r = Raw::new(m).unwrap();
        assert!(r.write_image(Image::Override, &picture(8, 6, 0x44)));
        m = r.dev;
    }
    for cut in [0, 1, 511, 512, 4095] {
        let mut mm = m.clone();
        mm.cut = Some(cut);
        let mut d = disk(mm);
        let p = plan::pick_picture(&mut d, &[(8, 6)]);
        assert!(!matches!(p, Pick::Override(_)), "cut {cut}: drawn without a durable removal");
    }
}

#[test]
fn the_splash_store_falls_back_to_the_default_and_skips_damage() {
    let mut r = Raw::new(both(&[])).unwrap();
    assert!(r.write_image(Image::Default, &picture(8, 6, 0x66)));
    let mut bad = picture(8, 6, 0x77);
    let n = bad.len();
    bad[n - 1] ^= 1; // the payload's CRC fails
    assert!(r.write_image(Image::Splash, &bad));
    let mut d = Disk { raw: r, log: Vec::new() };
    match plan::pick_picture(&mut d, &[(8, 6)]) {
        Pick::Splash(_, "default") => {}
        p => panic!("{p:?}"),
    }
    assert!(d.log.iter().any(|l| l.starts_with("IMAGE_SKIPPED")));
    // No region for the store: nothing, and nothing written.
    let mut m = Mem::blank();
    let t = build_table(SIZE as u64, &[(RECORD, 64 * K, 32 * K)]);
    m.bytes[..4096].copy_from_slice(&t);
    let mut d = disk(m);
    assert_eq!(plan::pick_picture(&mut d, &[(8, 6)]), Pick::Nothing);
    assert_eq!(d.raw.dev.writes, 0);
}

// ---- the probe report ----------------------------------------------------------------

#[test]
fn the_probe_report_is_written_in_its_region_alone_and_read_back() {
    let mut r = Raw::new(both(&[])).unwrap();
    let before = r.dev.bytes.clone();
    let mut p = b"format=1\n".to_vec();
    p.resize(PROBE_BYTES, b'\n');
    assert!(r.write_probe(&p));
    assert_eq!(r.read_probe().unwrap(), p);
    let (off, size) = r.region(PROBE).unwrap();
    let (off, size) = (off as usize, size as usize);
    assert_eq!(&r.dev.bytes[..off], &before[..off], "nothing before the region");
    assert_eq!(&r.dev.bytes[off + size..], &before[off + size..], "nothing after it");
    assert!(!r.write_probe(&vec![b'x'; PROBE_BYTES + 1]), "too long");
    r.dev.read_only = true;
    assert!(!r.write_probe(&p));
}

// ---- the loader image header ----------------------------------------------------------

#[test]
fn the_header_reads_back_and_every_damage_is_refused() {
    let region = 3 * M;
    let img = b"MZ not really a loader".to_vec();
    let h = loader_header(&img);
    assert_eq!(h.len(), 4096);
    assert_eq!(check_header(&h, region), Ok((img.len() as u64, crc32(&img))));
    for i in 0..32 {
        let mut b = h.clone();
        b[i] ^= 0x10;
        assert!(check_header(&b, region).is_err(), "byte {i}");
    }
    assert_eq!(check_header(&h[..31], region), Err(BadHeader::Short));
    assert_eq!(check_header(&vec![0u8; 4096], region), Err(BadHeader::Magic));
    // A size past the limit or past the region, with a good header CRC.
    let mut big = vec![0u8; LOADER_MAX as usize + 1];
    big[0] = 1;
    assert_eq!(check_header(&loader_header(&big), 64 * M), Err(BadHeader::Size));
    assert_eq!(check_header(&loader_header(&img), LOADER_HEADER as u64 + 5), Err(BadHeader::Size));
    assert_eq!(check_header(&loader_header(&[]), region), Err(BadHeader::Size));
    let _ = "".to_string();
}
