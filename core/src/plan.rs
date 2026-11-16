// SPDX-License-Identifier: GPL-2.0-or-later
//! What the loader does at a start (`boot-partition.md`, "What the loader does with
//! it"; 010 FR-006 to FR-016; 011 FR-012): read the record, use up the override and
//! pick the picture, pick the set, write the record, check the set and start it, and
//! fall back when something fails.
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::fmt;

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

macro_rules! plog {
    ($p:expr, $($t:tt)*) => { $p.log(format_args!($($t)*)) };
}

/// A checked set, its files in memory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Loaded {
    pub set: SetRec,
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
pub fn read_record<P: Platform>(p: &mut P) -> Option<InUse> {
    let mut slots: [Option<Vec<u8>>; 2] = [None, None];
    for (i, path) in SLOTS.iter().enumerate() {
        match p.read(path, SLOT_READ_MAX, None) {
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
fn write<P: Platform>(p: &mut P, cur: &mut InUse, changes: &[(&str, &str)]) -> bool {
    let (next, bytes) = match record::next(cur, changes) {
        Ok(x) => x,
        Err(e) => {
            plog!(p, "WRITE_FAILED: {:?}", e);
            return false;
        }
    };
    let path = SLOTS[next.slot];
    let ok = p.write_file(path, &bytes) && p.read(path, SLOT_READ_MAX, None).ok().as_deref() == Some(bytes.as_slice());
    if ok {
        plog!(p, "WRITE slot={} seq={} ok", slot_name(next.slot), next.seq);
        *cur = next;
    } else {
        plog!(p, "WRITE_FAILED slot={} seq={}", slot_name(next.slot), next.seq);
    }
    ok
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

fn load_image<P: Platform>(p: &mut P, path: &str) -> Option<Kind> {
    let t0 = p.now_ms();
    let b = match p.read(path, gspl::MAX_FILE, Some(t0 + IMAGE_MS)) {
        Ok(b) => b,
        Err(ReadErr::Missing) => return None,
        Err(e) => {
            plog!(p, "IMAGE_SKIPPED {}: {:?}", path, e);
            return None;
        }
    };
    let k = gspl::parse(&b);
    let t = p.now_ms() - t0;
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
/// FR-014). `modes`: the current mode of each display the loader owns.
pub fn pick_picture<P: Platform>(p: &mut P, modes: &[(u32, u32)]) -> Pick {
    if p.exists(OVERRIDE) {
        let k = load_image(p, OVERRIDE);
        if !p.remove(OVERRIDE) || p.exists(OVERRIDE) {
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
    for (path, name) in [(SPLASH, "splash"), (DEFAULT_SPLASH, "default")] {
        match load_image(p, path) {
            Some(Kind::Picture(pic)) => return Pick::Splash(pic, name),
            Some(Kind::LeaveScreen) => plog!(p, "IMAGE_SKIPPED {}: leave-screen is no splash", path),
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
    let kernel = read_checked(p, &format!("{base}\\vmlinuz"), set.kernel_size, &set.kernel_sha256)?;
    let initrd = match &set.initrd {
        Some((sha, size)) => Some(read_checked(p, &format!("{base}\\initrd"), *size, sha)?),
        None => None,
    };
    Ok(Loaded { set, kernel, initrd })
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

#[cfg(test)]
mod tests;
