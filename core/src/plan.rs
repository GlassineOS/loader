// SPDX-License-Identifier: GPL-2.0-or-later
//! What the loader does at a start (`boot-partition.md`, "What the loader does with
//! it"; 010 FR-006 to FR-016; 011 FR-012): read the record, use up the override and
//! pick the picture, pick the set, write the record, check the set and start it, and
//! fall back when something fails.
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::fmt;

use crate::answer::{Answer, Boot};
use crate::gspl::{self, Kind, Picture};
use crate::record::{self, InUse, SLOTS};
use crate::setrec::{self, SetRec};
use crate::sha256_hex;

/// The limit of unmarked starts, format 1.
pub const LIMIT: u64 = 3;
/// The wait for the recovery key, and after a start that wasn't marked healthy.
pub const WAIT_MS: u64 = 2000;
pub const WAIT_UNMARKED_MS: u64 = 5000;
/// Reading and checking one image (011 FR-005).
pub const IMAGE_MS: u64 = 1000;
/// The largest slot file read.
const SLOT_READ_MAX: u64 = 1 << 20;

pub const SETS: &str = "\\glassine\\sets";
pub const RECOVERY: &str = "\\glassine\\recovery";
pub const SPLASH: &str = "\\glassine\\splash\\splash.gspl";
pub const DEFAULT_SPLASH: &str = "\\glassine\\splash\\default.gspl";
pub const OVERRIDE: &str = "\\glassine\\splash\\override.gspl";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadErr {
    Missing,
    TooLarge(u64),
    TimedOut,
    Io,
}

/// What the loader needs of the firmware. Paths are absolute on the loader's own
/// volume, with backslashes.
pub trait Platform {
    /// A whole file of at most `max` bytes, given up after `deadline_ms` (by
    /// [`Platform::now_ms`]) when one is given.
    fn read(&mut self, path: &str, max: u64, deadline_ms: Option<u64>) -> Result<Vec<u8>, ReadErr>;
    /// Writes `bytes` as the whole file (created if missing, its size set to
    /// `bytes.len()`), then flushes it. `false` when any step failed.
    fn write_file(&mut self, path: &str, bytes: &[u8]) -> bool;
    /// Removes a file and flushes the volume. `true` only when both worked.
    fn remove(&mut self, path: &str) -> bool;
    fn exists(&mut self, path: &str) -> bool;
    /// The names of the directories in `path`.
    fn dirs(&mut self, path: &str) -> Vec<String>;
    fn now_ms(&mut self) -> u64;
    fn log(&mut self, line: fmt::Arguments);
}

/// Where the loader record's two slots are: the slot files on the loader's FAT volume
/// (a PC; every [`Platform`] is one), or the region `record` of a raw boot partition
/// ([`crate::raw::Raw`]).
pub trait RecordStore {
    /// The whole slot (`0` a, `1` b).
    fn read_slot(&mut self, slot: usize) -> Result<Vec<u8>, ReadErr>;
    /// Writes the slot's bytes and flushes them. `false` when any step failed. The
    /// caller reads the slot back.
    fn write_slot(&mut self, slot: usize, bytes: &[u8]) -> bool;
}

/// An image of the splash store (011; `docs/boot-partition.md`, "The splash store").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Image {
    Default,
    Splash,
    Override,
}

/// Where the splash store is: the files on the loader's FAT volume (every [`Platform`]
/// is one), or three regions of the raw form ([`crate::raw::Raw`]).
pub trait SplashStore {
    /// The whole image file, at most `max` bytes; [`ReadErr::Missing`] when there is
    /// none.
    fn read_image(&mut self, which: Image, max: u64, deadline_ms: Option<u64>) -> Result<Vec<u8>, ReadErr>;
    fn has_override(&mut self) -> bool;
    /// Removes the override durably. `true` only when it is known to be gone.
    fn remove_override(&mut self) -> bool;
    fn now_ms(&mut self) -> u64;
}

impl<P: Platform> SplashStore for P {
    fn read_image(&mut self, which: Image, max: u64, deadline_ms: Option<u64>) -> Result<Vec<u8>, ReadErr> {
        self.read(image_path(which), max, deadline_ms)
    }
    fn has_override(&mut self) -> bool {
        self.exists(OVERRIDE)
    }
    fn remove_override(&mut self) -> bool {
        self.remove(OVERRIDE) && !self.exists(OVERRIDE)
    }
    fn now_ms(&mut self) -> u64 {
        Platform::now_ms(self)
    }
}

/// The image's path in the FAT form.
pub fn image_path(which: Image) -> &'static str {
    match which {
        Image::Default => DEFAULT_SPLASH,
        Image::Splash => SPLASH,
        Image::Override => OVERRIDE,
    }
}

/// Where the loader's log lines go.
pub trait Log {
    fn log(&mut self, line: fmt::Arguments);
}

impl<P: Platform> RecordStore for P {
    fn read_slot(&mut self, slot: usize) -> Result<Vec<u8>, ReadErr> {
        self.read(SLOTS[slot], SLOT_READ_MAX, None)
    }
    fn write_slot(&mut self, slot: usize, bytes: &[u8]) -> bool {
        self.write_file(SLOTS[slot], bytes)
    }
}

impl<P: Platform> Log for P {
    fn log(&mut self, line: fmt::Arguments) {
        Platform::log(self, line)
    }
}

macro_rules! plog {
    ($p:expr, $($t:tt)*) => { Log::log($p, format_args!($($t)*)) };
}

/// A checked set, its files in memory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Loaded {
    pub set: SetRec,
    /// The kernel's path on the boot partition (`\glassine\sets\<id>\vmlinuz`), for the
    /// firmware: the image's file path when it is loaded from `kernel`.
    pub kernel_path: String,
    pub kernel: Vec<u8>,
    pub initrd: Option<Vec<u8>>,
}

/// Starts a kernel from memory. The firmware's start never comes back when it works;
/// it returns `false` when the kernel could not be started or returned, with whatever
/// it installed for the start gone. A test starter returns `true` to end the start.
pub trait Starter {
    fn start(&mut self, set: &Loaded, cmdline: &str) -> bool;
}

// ---- the record -----------------------------------------------------------------------

fn slot_name(i: usize) -> char {
    if i == 0 { 'a' } else { 'b' }
}

/// Reads both slots and returns the record in use.
pub fn read_record<S: RecordStore + Log>(p: &mut S) -> Option<InUse> {
    let mut slots: [Option<Vec<u8>>; 2] = [None, None];
    for i in 0..2 {
        match p.read_slot(i) {
            Ok(b) => {
                if let Err(e) = record::read_slot(&b) {
                    plog!(p, "SLOT {} not valid: {:?}", slot_name(i), e);
                }
                slots[i] = Some(b);
            }
            Err(e) => plog!(p, "SLOT {} not read: {:?}", slot_name(i), e),
        }
    }
    let r = record::in_use(slots[0].as_deref(), slots[1].as_deref());
    match &r {
        Some(u) => plog!(p, "RECORD slot={} seq={} both={}", slot_name(u.slot), u.seq, u.both_valid as u8),
        None => plog!(p, "NO_RECORD"),
    }
    r
}

/// The record's `unmarked`; empty is 0.
pub fn unmarked(r: &InUse) -> u64 {
    r.rec.get("unmarked").parse().unwrap_or(0)
}

/// How long the loader waits for the recovery key (010 FR-017).
pub fn wait_ms(r: Option<&InUse>) -> u64 {
    match r {
        Some(u) if unmarked(u) != 0 => WAIT_UNMARKED_MS,
        _ => WAIT_MS,
    }
}

/// Writes the next record to the slot not in use, flushes it and reads it back.
/// On success `cur` becomes the record written.
fn write<S: RecordStore + Log>(p: &mut S, cur: &mut InUse, changes: &[(&str, &str)]) -> bool {
    let (next, bytes) = match record::next(cur, changes) {
        Ok(x) => x,
        Err(e) => {
            plog!(p, "WRITE_FAILED: {:?}", e);
            return false;
        }
    };
    let ok = p.write_slot(next.slot, &bytes) && p.read_slot(next.slot).ok().as_deref() == Some(bytes.as_slice());
    if ok {
        plog!(p, "WRITE slot={} seq={} ok", slot_name(next.slot), next.seq);
        *cur = next;
    } else {
        plog!(p, "WRITE_FAILED slot={} seq={}", slot_name(next.slot), next.seq);
    }
    ok
}

/// Writes `changes` over the record in use as one write of the slot not in use (flushed,
/// read back); on success `cur` becomes the record written. For writes outside
/// [`start`] and [`handback`]: the start menu's (spec 004).
pub fn write_changes<S: RecordStore + Log>(p: &mut S, cur: &mut InUse, changes: &[(&str, &str)]) -> bool {
    write(p, cur, changes)
}

// ---- the splash ------------------------------------------------------------------------

/// What to draw.
#[derive(Debug, PartialEq, Eq)]
pub enum Pick {
    Override(Picture),
    LeaveScreen,
    /// The splash, and which file it came from.
    Splash(Picture, &'static str),
    Nothing,
}

fn load_image<S: SplashStore + Log>(p: &mut S, which: Image) -> Option<Kind> {
    let path = image_path(which);
    let t0 = SplashStore::now_ms(p);
    let b = match p.read_image(which, gspl::MAX_FILE, Some(t0 + IMAGE_MS)) {
        Ok(b) => b,
        Err(ReadErr::Missing) => return None,
        Err(e) => {
            plog!(p, "IMAGE_SKIPPED {}: {:?}", path, e);
            return None;
        }
    };
    let k = gspl::parse(&b);
    let t = SplashStore::now_ms(p) - t0;
    match k {
        Ok(_) if t > IMAGE_MS => {
            plog!(p, "IMAGE_SKIPPED {}: time limit ({} ms)", path, t);
            None
        }
        Ok(k) => Some(k),
        Err(e) => {
            plog!(p, "IMAGE_SKIPPED {}: {:?}", path, e);
            None
        }
    }
}

/// Uses up the override and picks the picture (011 FR-002, FR-008, FR-010, FR-012 to
/// FR-014). `modes`: the current mode of each display the loader owns. Works on either
/// form of the boot partition.
pub fn pick_picture<S: SplashStore + Log>(p: &mut S, modes: &[(u32, u32)]) -> Pick {
    if p.has_override() {
        let k = load_image(p, Image::Override);
        if !p.remove_override() {
            plog!(p, "OVERRIDE_NOT_REMOVED: not drawn");
        } else {
            plog!(p, "OVERRIDE_REMOVED");
            match (k, modes) {
                (_, []) => plog!(p, "override not drawn: no display"),
                (Some(_), [_, _, ..]) => plog!(p, "override not drawn: {} displays", modes.len()),
                (Some(Kind::LeaveScreen), [_]) => return Pick::LeaveScreen,
                (Some(Kind::Picture(pic)), [m]) if pic.mode == *m => return Pick::Override(pic),
                (Some(Kind::Picture(pic)), [m]) => {
                    plog!(p, "override not drawn: made for {}x{}, the display is {}x{}", pic.mode.0, pic.mode.1, m.0, m.1)
                }
                (None, _) => {}
            }
        }
    }
    if modes.is_empty() {
        return Pick::Nothing;
    }
    for (which, name) in [(Image::Splash, "splash"), (Image::Default, "default")] {
        match load_image(p, which) {
            Some(Kind::Picture(pic)) => return Pick::Splash(pic, name),
            Some(Kind::LeaveScreen) => plog!(p, "IMAGE_SKIPPED {}: leave-screen is no splash", image_path(which)),
            None => {}
        }
    }
    Pick::Nothing
}

// ---- the sets ------------------------------------------------------------------------

/// Checks the set in `dir\id` and reads its files (boot-partition.md, "How the loader
/// checks and starts a set"). `want_rec`: the sha256 the loader record names for its
/// `set.rec`; none for a recovery found by itself.
pub fn check_set<P: Platform>(p: &mut P, dir: &str, id: &str, want_rec: Option<&str>, recovery: bool) -> Result<Loaded, String> {
    let base = format!("{dir}\\{id}");
    let rec = p.read(&format!("{base}\\set.rec"), setrec::SET_REC_MAX, None).map_err(|e| format!("set.rec: {e:?}"))?;
    if let Some(w) = want_rec {
        if sha256_hex(&rec) != w {
            return Err("set.rec isn't the one the loader record names".into());
        }
    }
    let set = setrec::parse(&rec, id, recovery).map_err(|e| format!("set.rec: {e:?}"))?;
    let kernel_path = format!("{base}\\vmlinuz");
    let kernel = read_checked(p, &kernel_path, set.kernel_size, &set.kernel_sha256)?;
    let initrd = match &set.initrd {
        Some((sha, size)) => Some(read_checked(p, &format!("{base}\\initrd"), *size, sha)?),
        None => None,
    };
    Ok(Loaded { set, kernel_path, kernel, initrd })
}

fn read_checked<P: Platform>(p: &mut P, path: &str, size: u64, sha: &str) -> Result<Vec<u8>, String> {
    let t0 = p.now_ms();
    let b = p.read(path, size, None).map_err(|e| format!("{path}: {e:?}"))?;
    if b.len() as u64 != size {
        return Err(format!("{path}: {} bytes, the record says {size}", b.len()));
    }
    let got = sha256_hex(&b);
    let t = p.now_ms() - t0;
    plog!(p, "read and summed {} ({} bytes) in {} ms", path, b.len(), t);
    if got != sha {
        return Err(format!("{path}: sha256 {got}, the record says {sha}"));
    }
    Ok(b)
}

// ---- the start -----------------------------------------------------------------------

/// The person's choice in the list (010 FR-039 to FR-042).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Choice {
    /// No key, or "GlassineOS": what the record and the count say.
    Ordinary,
    /// The same, with this one-shot deployment request.
    Deploy(String),
    PreviousKernel,
    Recovery,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Cand {
    Trial,
    Default,
    Previous,
    Recovery,
}
const ORDER: [Cand; 4] = [Cand::Trial, Cand::Default, Cand::Previous, Cand::Recovery];

/// Starts what this start should start. `false`: nothing could be started; the caller
/// then shows the message of 010 FR-016 and returns to the firmware.
pub fn start<P: Platform, S: Starter>(p: &mut P, s: &mut S, rec: Option<InUse>, choice: Choice) -> bool {
    let Some(mut cur) = rec else {
        plog!(p, "no loader record: the recovery");
        if start_recovery(p, s, None) {
            return true;
        }
        plog!(p, "NOTHING_STARTED");
        return false;
    };
    let raw_request = cur.rec.get("request").to_string();
    let request = if cur.both_valid { raw_request.as_str() } else { "" };
    if !cur.both_valid && !raw_request.is_empty() {
        plog!(p, "request={} not taken: only one slot is valid", raw_request);
    }
    let deploy = match &choice {
        Choice::Deploy(id) => Some(id.clone()),
        _ => None,
    };
    let count = unmarked(&cur);
    let by_count = || match count {
        c if c < LIMIT => (Cand::Default, "default"),
        LIMIT => (Cand::Previous, "limit"),
        _ => (Cand::Recovery, ""),
    };
    let (mut first, mut why) = match (&choice, request) {
        (Choice::PreviousKernel, _) => (Cand::Previous, "choice"),
        (Choice::Recovery, _) => (Cand::Recovery, ""),
        (_, "trial") => (Cand::Trial, "request"),
        (_, "recovery") => (Cand::Recovery, ""),
        _ => by_count(),
    };
    if first == Cand::Previous && cur.rec.get("previous").is_empty() && why == "limit" {
        plog!(p, "the limit is reached and there is no previous set");
        first = Cand::Recovery;
    }
    plog!(p, "PICK {:?} why={} unmarked={} request={} choice={:?}", first, why, count, request, choice);

    let mut writable = true;
    let take = match (&choice, request) {
        (Choice::Ordinary | Choice::Deploy(_), "trial" | "recovery") => true,
        (Choice::Recovery, "recovery") => true,
        _ => false,
    };
    if take {
        let trial = cur.rec.get("trial").to_string();
        let mut ch: Vec<(&str, &str)> = alloc::vec![("request", "")];
        if request == "trial" {
            ch.push(("started", &trial));
            ch.push(("started_why", "request"));
        }
        if write(p, &mut cur, &ch) {
            plog!(p, "REQUEST_TAKEN {}", request);
        } else {
            plog!(p, "REQUEST_NOT_TAKEN {}: the record can't be written; the default set", request);
            writable = false;
            (first, why) = (Cand::Default, "default");
        }
    }

    let mut counted = false;
    let mut i = ORDER.iter().position(|c| *c == first).unwrap();
    while i < ORDER.len() {
        let cand = ORDER[i];
        i += 1;
        if cand == Cand::Recovery {
            if start_recovery(p, s, Some(&cur)) {
                return true;
            }
            break;
        }
        let (key, why_here) = match cand {
            Cand::Trial => ("trial", "request"),
            Cand::Default => ("default", "default"),
            _ => ("previous", if cand == first { why } else { "limit" }),
        };
        let id = cur.rec.get(key).to_string();
        if id.is_empty() {
            plog!(p, "no {} set", key);
            continue;
        }
        let want = cur.rec.get(&format!("{key}_rec")).to_string();
        let loaded = match check_set(p, SETS, &id, Some(&want), false) {
            Ok(l) => {
                plog!(p, "CHECK {} {} ok", key, id);
                l
            }
            Err(e) => {
                plog!(p, "CHECK_FAILED {} {}: {}", key, id, e);
                continue;
            }
        };
        if cand != Cand::Trial && writable && !counted {
            let n = (count.saturating_add(1)).to_string();
            let mut ch: Vec<(&str, &str)> = alloc::vec![("unmarked", &n), ("started", &id), ("started_why", why_here)];
            if !cur.both_valid {
                ch.push(("request", ""));
            }
            if write(p, &mut cur, &ch) {
                counted = true;
            } else {
                writable = false;
                if cand != Cand::Default {
                    plog!(p, "the record can't be written: the default set, uncounted");
                    i = 1;
                    continue;
                }
                plog!(p, "the record can't be written: the default set starts uncounted");
            }
        }
        let cmd = setrec::start_cmdline(&loaded.set, deploy.as_deref());
        plog!(p, "START {} {} why={} cmdline=\"{}\"", key, id, why_here, cmd);
        if s.start(&loaded, &cmd) {
            return true;
        }
        plog!(p, "START_RETURNED {} {}", key, id);
        // Nothing of this set stays in memory for the next one.
        drop(loaded);
    }
    plog!(p, "NOTHING_STARTED");
    false
}

/// The recovery: the one the record names, else each in `\glassine\recovery\` in
/// descending byte order of its name, the first that checks.
fn start_recovery<P: Platform, S: Starter>(p: &mut P, s: &mut S, cur: Option<&InUse>) -> bool {
    let mut tried: Option<String> = None;
    if let Some(u) = cur {
        let id = u.rec.get("recovery").to_string();
        let want = u.rec.get("recovery_rec").to_string();
        if !id.is_empty() {
            match check_set(p, RECOVERY, &id, Some(&want), true) {
                Ok(l) => {
                    plog!(p, "CHECK recovery {} ok", id);
                    plog!(p, "START recovery {} cmdline=\"{}\"", id, l.set.cmdline);
                    if s.start(&l, &l.set.cmdline) {
                        return true;
                    }
                    plog!(p, "START_RETURNED recovery {}", id);
                }
                Err(e) => plog!(p, "CHECK_FAILED recovery {}: {}", id, e),
            }
            tried = Some(id);
        }
    }
    let mut names = p.dirs(RECOVERY);
    names.sort_unstable_by(|a, b| b.as_bytes().cmp(a.as_bytes()));
    for id in names {
        if tried.as_deref() == Some(id.as_str()) {
            continue;
        }
        match check_set(p, RECOVERY, &id, None, true) {
            Ok(l) => {
                plog!(p, "CHECK recovery {} ok (found by itself)", id);
                plog!(p, "START recovery {} cmdline=\"{}\"", id, l.set.cmdline);
                if s.start(&l, &l.set.cmdline) {
                    return true;
                }
                plog!(p, "START_RETURNED recovery {}", id);
            }
            Err(e) => plog!(p, "CHECK_FAILED recovery {}: {}", id, e),
        }
    }
    false
}

// ---- under a parent bootloader ------------------------------------------------------------

/// What the loader answers a parent bootloader (GlassineOS/boot spec 012, FR-010 to
/// FR-013): the choice of a start as [`start`] makes it, with the same writes to the
/// record, but naming the set instead of checking and starting it. The default set is
/// the parent's baseline when the record names none; the recovery is the parent's.
/// `pick_recovery`: the person asked for the recovery at power-on (`glassine.pick=`).
pub fn handback<S: RecordStore + Log>(p: &mut S, rec: Option<InUse>, pick_recovery: bool) -> Answer {
    handback_with(p, rec, pick_recovery, &[], &[])
}

/// [`handback`] with loader-owned keys added to its writes: `any` to every write it
/// makes, `counted` only to the write that counts the start (the default or the
/// previous set). The decision is the same. A `deploy_once` the record holds (a start
/// menu's pick whose start never reached the initramfs) is cleared by every write: only
/// the pick's own write sets it (boot `docs/handback.md`).
pub fn handback_with<S: RecordStore + Log>(
    p: &mut S,
    rec: Option<InUse>,
    pick_recovery: bool,
    any: &[(&str, &str)],
    counted: &[(&str, &str)],
) -> Answer {
    let stale_once = rec.as_ref().is_some_and(|c| !c.rec.get("deploy_once").is_empty());
    let write = |p: &mut S, cur: &mut InUse, ch: &[(&str, &str)], count: bool| {
        let mut all: Vec<(&str, &str)> = ch.to_vec();
        all.extend_from_slice(any);
        if stale_once {
            all.push(("deploy_once", ""));
        }
        if count {
            all.extend_from_slice(counted);
        }
        write(p, cur, &all)
    };
    let Some(mut cur) = rec else {
        plog!(p, "no loader record: the baseline");
        return Answer::new(Boot::Baseline, "no-record");
    };
    let raw_request = cur.rec.get("request").to_string();
    let request = if cur.both_valid { raw_request.as_str() } else { "" };
    if !cur.both_valid && !raw_request.is_empty() {
        plog!(p, "request={} not taken: only one slot is valid", raw_request);
    }
    let set = |cur: &InUse, key: &str| -> Option<Boot> {
        let id = cur.rec.get(key);
        (!id.is_empty()).then(|| Boot::Set { id: id.into(), rec: cur.rec.get(&format!("{key}_rec")).into() })
    };

    // The person's recovery, or a request: cleared before the answer (010 FR-009).
    let take = match (pick_recovery, request) {
        (false, "trial" | "recovery") => true,
        (true, "recovery") => true,
        _ => false,
    };
    if pick_recovery {
        if take && !write(p, &mut cur, &[("request", "")], false) {
            plog!(p, "the record can't be written; the recovery all the same (it isn't counted)");
        }
        plog!(p, "ANSWER recovery why=choice");
        return Answer::new(Boot::Recovery, "choice");
    }
    if take {
        let trial = cur.rec.get("trial").to_string();
        let mut ch: Vec<(&str, &str)> = alloc::vec![("request", "")];
        if request == "trial" && !trial.is_empty() {
            ch.push(("started", &trial));
            ch.push(("started_why", "request"));
        }
        if !write(p, &mut cur, &ch, false) {
            plog!(p, "REQUEST_NOT_TAKEN {}: the record can't be written; the baseline", request);
            return Answer::new(Boot::Baseline, "not-written");
        }
        plog!(p, "REQUEST_TAKEN {}", request);
        if request == "recovery" {
            plog!(p, "ANSWER recovery why=request");
            return Answer::new(Boot::Recovery, "request");
        }
        if let Some(b) = set(&cur, "trial") {
            plog!(p, "ANSWER {:?} why=request", b);
            return Answer::new(b, "request");
        }
        plog!(p, "a trial request with no trial set: by the count");
    }

    let count = unmarked(&cur);
    let (key, why) = match count {
        c if c < LIMIT => ("default", "default"),
        LIMIT if !cur.rec.get("previous").is_empty() => ("previous", "limit"),
        _ => {
            plog!(p, "ANSWER recovery why=limit unmarked={}", count);
            return Answer::new(Boot::Recovery, "limit");
        }
    };
    let boot = set(&cur, key).unwrap_or(Boot::Baseline);
    let id = cur.rec.get(key).to_string();
    let n = count.saturating_add(1).to_string();
    let mut ch: Vec<(&str, &str)> = alloc::vec![("unmarked", &n), ("started", &id), ("started_why", why)];
    if !cur.both_valid {
        ch.push(("request", ""));
    }
    if !write(p, &mut cur, &ch, true) {
        // 010 FR-009: a loader that can't write starts the default set, uncounted.
        let b = set(&cur, "default").unwrap_or(Boot::Baseline);
        plog!(p, "the record can't be written: {:?}, uncounted", b);
        return Answer::new(b, "not-written");
    }
    plog!(p, "ANSWER {:?} why={} unmarked={}", boot, why, n);
    Answer::new(boot, why)
}

/// What a hand-back start with `display=epd-fw` does about the firmware's panel refresh
/// (loader spec 003 FR-009, FR-010; boot `docs/handback.md`): whether it may draw, and
/// the loader-owned keys its record writes carry.
///
/// - `display_failed` set: no draw, whatever `unmarked` says; only the running system
///   clears it.
/// - `unmarked` above 0 at entry: no draw; when the start before drew (`drew=1`), that
///   draw may have hung: `display_failed=<unmarked>` is set, sticky.
/// - else: the draw, on a start counted before it (the answer `default`), and the
///   counting write says `drew=1`.
///
/// Every write of a start that doesn't draw clears `drew`.
#[derive(Debug, PartialEq, Eq)]
pub struct EpdPlan {
    /// The draw, if the start is counted.
    pub draw: bool,
    /// Why not, for the log.
    pub skip: Option<String>,
    /// Keys for every write.
    pub any: Vec<(&'static str, String)>,
    /// Keys for the counting write only.
    pub counted: Vec<(&'static str, String)>,
}

pub fn epd_plan(rec: Option<&InUse>) -> EpdPlan {
    let Some(r) = rec else {
        return EpdPlan { draw: false, skip: Some("no record".into()), any: Vec::new(), counted: Vec::new() };
    };
    let failed = r.rec.get("display_failed");
    let drew = r.rec.get("drew") == "1";
    let clear_drew = || if r.rec.get("drew").is_empty() { Vec::new() } else { alloc::vec![("drew", String::new())] };
    if !failed.is_empty() {
        return EpdPlan { draw: false, skip: Some(format!("display_failed={failed}")), any: clear_drew(), counted: Vec::new() };
    }
    let n = unmarked(r);
    if n > 0 && drew {
        return EpdPlan {
            draw: false,
            skip: Some(format!("the previous start drew and wasn't marked healthy: display_failed={n}")),
            any: alloc::vec![("display_failed", n.to_string()), ("drew", String::new())],
            counted: Vec::new(),
        };
    }
    if n > 0 {
        return EpdPlan { draw: false, skip: Some("the previous start wasn't marked healthy".into()), any: clear_drew(), counted: Vec::new() };
    }
    EpdPlan { draw: true, skip: None, any: clear_drew(), counted: alloc::vec![("drew", "1".into())] }
}

/// Whether the start of `answer` draws under `plan`: only a counted start (`default`).
pub fn epd_draws(plan: &EpdPlan, answer: &Answer) -> Result<(), String> {
    match (&plan.skip, answer.why) {
        (Some(why), _) => Err(why.clone()),
        (None, "default") if plan.draw => Ok(()),
        _ => Err("the start isn't counted".into()),
    }
}

/// The region the firmware variable `DisplaySplashBufferInfo` names (boot
/// `docs/handback.md`, "A display through the firmware's refresh"): exactly 12 bytes,
/// three little-endian u32 words, version 1, then the base and the size in bytes. Any
/// other shape names no region.
pub fn splash_buffer_region(var: &[u8]) -> Option<(u64, u64)> {
    let w: &[u8; 12] = var.try_into().ok()?;
    let word = |i: usize| u32::from_le_bytes([w[i], w[i + 1], w[i + 2], w[i + 3]]) as u64;
    (word(0) == 1).then(|| (word(4), word(8)))
}

/// Which region bounds the `epd-fw` picture.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum EpdBound {
    /// The framebuffer the GOP's Mode names.
    Mode,
    /// The region `DisplaySplashBufferInfo` names.
    SplashBufferInfo,
}

/// Whether `len` bytes from `at` lie inside `base`+`size`.
fn inside(at: u64, len: u64, base: u64, size: u64) -> bool {
    match (at.checked_add(len), base.checked_add(size)) {
        (Some(end), Some(lim)) => at >= base && end <= lim,
        _ => false,
    }
}

/// The bound of the `epd-fw` picture (spec 003 FR-002): the `w`×`h`×2 bytes from `fb`
/// inside the Mode's framebuffer (`mode`: base, size), else inside the region of
/// `DisplaySplashBufferInfo` (`var`: its bytes, if the firmware has it). The caller has
/// checked the status, a non-zero `fb`, `w` and `h`, and `w` a multiple of 8.
pub fn epd_bound(fb: u64, w: u32, h: u32, mode: Option<(u64, u64)>, var: Option<&[u8]>) -> Result<EpdBound, String> {
    let bytes = (w as u64) * (h as u64) * 2;
    if mode.is_some_and(|(base, size)| inside(fb, bytes, base, size)) {
        return Ok(EpdBound::Mode);
    }
    let region = var.and_then(splash_buffer_region);
    if region.is_some_and(|(base, size)| inside(fb, bytes, base, size)) {
        return Ok(EpdBound::SplashBufferInfo);
    }
    let m = mode.map_or("no Mode".into(), |(b, s)| format!("the Mode's 0x{b:x}+{s}"));
    let v = match (var, region) {
        (None, _) => "no DisplaySplashBufferInfo".into(),
        (Some(d), None) => format!("a DisplaySplashBufferInfo of {} bytes that names no region", d.len()),
        (Some(_), Some((b, s))) => format!("DisplaySplashBufferInfo's 0x{b:x}+{s}"),
    };
    Err(format!("framebuffer 0x{fb:x}+{bytes} outside {m} and {v}"))
}

#[cfg(test)]
mod tests;
