// SPDX-License-Identifier: GPL-2.0-or-later
//! The start menu on a firmware-refreshed panel (loader spec 004; boot
//! `docs/handback.md`, "The start menu"): when it opens, its writes, what each pick
//! answers and writes, the refresh rectangles, the touch mapping and its debounce.
//! Nothing here calls the firmware.
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::answer::{Answer, Boot};
use crate::plan::{self, Choice, EpdPlan, LIMIT};
use crate::record::InUse;

/// How long the menu waits for input before it takes the GlassineOS pick.
pub const TIMEOUT_MS: u64 = 60_000;
/// The firmware watchdog the loader re-arms while the menu is open, and leaves armed
/// when it answers.
pub const WATCHDOG_S: usize = 30;
/// The longest wait between two re-arms.
pub const REARM_MS: u64 = 10_000;

// ---- when it opens ----------------------------------------------------------------------

/// Why the menu opens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Why {
    /// `request=menu`, read with both slots valid.
    Request,
    /// `unmarked` above 0 at entry.
    Unmarked,
}

impl Why {
    pub fn name(self) -> &'static str {
        match self {
            Why::Request => "request",
            Why::Unmarked => "unmarked",
        }
    }
}

/// Whether the record asks for the menu at this start (spec 004 FR-001), before the
/// display is looked at: `ep` is the start's [`plan::epd_plan`]. A start whose last draw
/// may have hung (`drew=1` with `unmarked` above 0), or with `display_failed`, never
/// opens it; nor a start where the parent passed the recovery pick.
pub fn opens(rec: Option<&InUse>, ep: &EpdPlan, pick_recovery: bool) -> Option<Why> {
    let r = rec?;
    if pick_recovery || ep.any.iter().any(|(k, _)| *k == "display_failed") || !r.rec.get("display_failed").is_empty() {
        return None;
    }
    if r.both_valid && r.rec.get("request") == "menu" {
        return Some(Why::Request);
    }
    (plan::unmarked(r) > 0).then_some(Why::Unmarked)
}

/// The menu write (FR-003), made before the first refresh: the request cleared when it
/// was `menu`, the start counted, `drew=1`.
pub fn open_write(rec: &InUse) -> Vec<(&'static str, String)> {
    let mut ch: Vec<(&'static str, String)> = Vec::new();
    if rec.rec.get("request") == "menu" {
        ch.push(("request", String::new()));
    }
    ch.push(("unmarked", plan::unmarked(rec).saturating_add(1).to_string()));
    ch.push(("drew", "1".into()));
    if !rec.rec.get("deploy_once").is_empty() {
        ch.push(("deploy_once", String::new()));
    }
    ch
}

// ---- what a pick answers and writes -----------------------------------------------------

/// What a start answers for a choice and the changes of its one write, computed from the
/// record as read at entry (boot `docs/handback.md`, "The start menu"). `counted`: the
/// write raises `unmarked`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decision {
    pub answer: Answer,
    pub changes: Vec<(&'static str, String)>,
    pub counted: bool,
}

fn set_of(cur: &InUse, key: &str) -> Option<Boot> {
    let id = cur.rec.get(key);
    (!id.is_empty()).then(|| Boot::Set { id: id.into(), rec: cur.rec.get(&format!("{key}_rec")).into() })
}

/// The decision of a hand-back start for `choice`, as [`plan::handback`] makes it for
/// the ordinary start (one write instead of a request write and a count write), and as
/// the PC's [`plan::start`] makes it for the other picks: the previous kernel counted with
/// `started_why=choice`, the recovery not counted, a deployment the ordinary start with
/// `glassine.deploy.once=<id>`. `request=menu` is no request here.
///
/// Under a parent that passes no command line (`parent_cmdline=no`, the board's
/// `LOADER_PARENT_CMDLINE`, boot `docs/handback.md`) the deployment goes into the
/// record instead: `deploy_once=<id>` in the pick's write, the answer without
/// `cmdline_add`; the initramfs takes it from there and clears it. Any other pick
/// clears a `deploy_once` the record still holds.
pub fn decide(cur: &InUse, choice: &Choice) -> Decision {
    let mut d = decide_args(cur, choice);
    if cur.rec.get("parent_cmdline") == "no" {
        if let Some(w) = d.answer.cmdline_add.take() {
            d.changes.push(("deploy_once", w.trim_start_matches("glassine.deploy.once=").into()));
            return d;
        }
    }
    if !cur.rec.get("deploy_once").is_empty() {
        d.changes.push(("deploy_once", String::new()));
    }
    d
}

fn decide_args(cur: &InUse, choice: &Choice) -> Decision {
    let request = if cur.both_valid { cur.rec.get("request") } else { "" };
    let n = plan::unmarked(cur);
    let mut changes: Vec<(&'static str, String)> = Vec::new();
    let count = |changes: &mut Vec<(&'static str, String)>, id: &str, why: &str| {
        changes.push(("unmarked", n.saturating_add(1).to_string()));
        changes.push(("started", id.into()));
        changes.push(("started_why", why.into()));
        if !cur.both_valid {
            changes.push(("request", String::new()));
        }
    };
    let deploy = match choice {
        Choice::Deploy(id) => Some(id.clone()),
        _ => None,
    };
    let with_deploy = |mut a: Answer| {
        if a.boot != Boot::Recovery {
            a.cmdline_add = deploy.as_ref().map(|id| format!("glassine.deploy.once={id}"));
        }
        a
    };
    match choice {
        Choice::Recovery => {
            if request == "recovery" {
                changes.push(("request", String::new()));
            }
            return Decision { answer: Answer::new(Boot::Recovery, "choice"), changes, counted: false };
        }
        Choice::PreviousKernel => {
            if let Some(b) = set_of(cur, "previous") {
                let id = cur.rec.get("previous").to_string();
                count(&mut changes, &id, "choice");
                return Decision { answer: Answer::new(b, "choice"), changes, counted: true };
            }
        }
        _ => {}
    }
    if request == "recovery" {
        changes.push(("request", String::new()));
        return Decision { answer: Answer::new(Boot::Recovery, "request"), changes, counted: false };
    }
    if request == "trial" {
        changes.push(("request", String::new()));
        if let Some(b) = set_of(cur, "trial") {
            changes.push(("started", cur.rec.get("trial").into()));
            changes.push(("started_why", "request".into()));
            return Decision { answer: with_deploy(Answer::new(b, "request")), changes, counted: false };
        }
    }
    let (key, why) = match n {
        c if c < LIMIT => ("default", "default"),
        LIMIT if !cur.rec.get("previous").is_empty() => ("previous", "limit"),
        _ => return Decision { answer: Answer::new(Boot::Recovery, "limit"), changes, counted: false },
    };
    let boot = set_of(cur, key).unwrap_or(Boot::Baseline);
    let id = cur.rec.get(key).to_string();
    count(&mut changes, &id, why);
    Decision { answer: with_deploy(Answer::new(boot, why)), changes, counted: true }
}

/// The pick's write after the menu (FR-005): the decision's changes from the record read
/// at entry, `unmarked` written explicitly (the menu write raised it) and `drew` cleared.
pub fn pick_write(entry: &InUse, d: &Decision) -> Vec<(&'static str, String)> {
    let n = plan::unmarked(entry);
    let mut ch: Vec<(&'static str, String)> = alloc::vec![("unmarked", (if d.counted { n + 1 } else { n }).to_string())];
    for (k, v) in &d.changes {
        ch.retain(|(x, _)| x != k);
        ch.push((k, v.clone()));
    }
    ch.push(("drew", String::new()));
    ch
}

/// The answer when the pick's write fails (FR-005): the recovery for the recovery pick,
/// else the baseline.
pub fn not_written(choice: &Choice) -> Answer {
    match choice {
        Choice::Recovery => Answer::new(Boot::Recovery, "choice"),
        _ => Answer::new(Boot::Baseline, "not-written"),
    }
}

// ---- refresh rectangles -----------------------------------------------------------------

/// A rectangle, `x` and `y` its top left corner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

impl Rect {
    fn x1(&self) -> u32 {
        self.x + self.w
    }
    fn y1(&self) -> u32 {
        self.y + self.h
    }
    /// Overlapping or touching.
    fn meets(&self, o: &Rect) -> bool {
        self.x <= o.x1() && o.x <= self.x1() && self.y <= o.y1() && o.y <= self.y1()
    }
    fn union(&self, o: &Rect) -> Rect {
        let (x, y) = (self.x.min(o.x), self.y.min(o.y));
        Rect { x, y, w: self.x1().max(o.x1()) - x, h: self.y1().max(o.y1()) - y }
    }
}

/// The most rectangles one refresh round sends: one, the bounding box of what changed,
/// so a change is one refresh of the panel (spec 004 FR-009).
pub const MAX_RECTS: usize = 1;

/// Merges overlapping or touching rectangles until none meet; more than `max` left:
/// their bounding box. Empty rectangles are dropped.
pub fn merge(rs: &[Rect], max: usize) -> Vec<Rect> {
    let mut out: Vec<Rect> = rs.iter().copied().filter(|r| r.w > 0 && r.h > 0).collect();
    loop {
        let mut joined = false;
        'scan: for i in 0..out.len() {
            for j in i + 1..out.len() {
                if out[i].meets(&out[j]) {
                    out[i] = out[i].union(&out[j]);
                    out.remove(j);
                    joined = true;
                    break 'scan;
                }
            }
        }
        if !joined {
            break;
        }
    }
    if out.len() > max {
        let b = out.iter().skip(1).fold(out[0], |a, r| a.union(r));
        return alloc::vec![b];
    }
    out
}

/// The panel's native rectangle of portrait rectangle `r` (FR-009): portrait (u, v) is
/// native (W − 1 − v, u), so (u, v, w, h) is (W − v − h, u, h, w); x rounded down and
/// x + width up to multiples of 8; clipped to the native `w`×`h` panel. `None`: nothing
/// of it is on the panel.
pub fn native(r: Rect, w: u32, h: u32) -> Option<Rect> {
    // Clip in portrait first: the portrait panel is h wide, w high.
    let u0 = r.x.min(h);
    let u1 = r.x.saturating_add(r.w).min(h);
    let v0 = r.y.min(w);
    let v1 = r.y.saturating_add(r.h).min(w);
    if u0 >= u1 || v0 >= v1 {
        return None;
    }
    let x0 = (w - v1) & !7;
    let x1 = ((w - v0 + 7) & !7).min(w);
    Some(Rect { x: x0, y: u0, w: x1 - x0, h: u1 - u0 })
}

// ---- touch ------------------------------------------------------------------------------

/// How the pointer's axes lie on the panel's native axes: X and Y swapped first, then
/// each flipped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Orient {
    pub swap: bool,
    pub flip_x: bool,
    pub flip_y: bool,
}

/// The pointer's ranges (the Mode's `AbsoluteMinX` … `AbsoluteMaxY`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Range {
    pub min_x: u64,
    pub max_x: u64,
    pub min_y: u64,
    pub max_y: u64,
}

impl Range {
    /// Usable: each maximum above its minimum.
    pub fn usable(&self) -> bool {
        self.max_x > self.min_x && self.max_y > self.min_y
    }
}

fn scale(v: u64, min: u64, max: u64, out: u32) -> u32 {
    let v = v.clamp(min, max) - min;
    ((v as u128 * (out as u128 - 1)) / ((max - min) as u128)) as u32
}

/// The portrait position (u, v) of pointer position (`x`, `y`) on the native `w`×`h`
/// panel (FR-011): through the ranges to native, then by the inverse of the drawing's
/// mapping (native (x, y) is portrait (y, W − 1 − x)). `None` for unusable ranges.
pub fn to_portrait(x: u64, y: u64, r: Range, o: Orient, w: u32, h: u32) -> Option<(u32, u32)> {
    if !r.usable() || w == 0 || h == 0 {
        return None;
    }
    // Normalised to native: the pointer's X (or Y when swapped) along the native x.
    let (along_x, along_y) = if o.swap {
        ((y, r.min_y, r.max_y), (x, r.min_x, r.max_x))
    } else {
        ((x, r.min_x, r.max_x), (y, r.min_y, r.max_y))
    };
    let mut nx = scale(along_x.0, along_x.1, along_x.2, w);
    let mut ny = scale(along_y.0, along_y.1, along_y.2, h);
    if o.flip_x {
        nx = w - 1 - nx;
    }
    if o.flip_y {
        ny = h - 1 - ny;
    }
    Some((ny, w - 1 - nx))
}

/// The pointer position of portrait (u, v): the inverse of [`to_portrait`], for tests and
/// for the stand-in's scripted taps.
pub fn from_portrait(u: u32, v: u32, r: Range, o: Orient, w: u32, h: u32) -> (u64, u64) {
    let mut nx = w - 1 - v;
    let mut ny = u;
    if o.flip_x {
        nx = w - 1 - nx;
    }
    if o.flip_y {
        ny = h - 1 - ny;
    }
    // The middle of the pointer's step, so the round trip is exact.
    let unscale = |n: u32, min: u64, max: u64, out: u32| -> u64 {
        let span = (max - min) as u128;
        min + ((n as u128 * span + (out as u128 - 1) - 1) / (out as u128 - 1)).min(span) as u64
    };
    if o.swap {
        (unscale(ny, r.min_x, r.max_x, h), unscale(nx, r.min_y, r.max_y, w))
    } else {
        (unscale(nx, r.min_x, r.max_x, w), unscale(ny, r.min_y, r.max_y, h))
    }
}

/// A release shorter than this is a bounce: the press goes on.
pub const BOUNCE_MS: u64 = 30;
/// No new state for this long after a press is a release (a device that never reports
/// one); a press this soon after a release is ignored.
pub const QUIET_MS: u64 = 300;

/// The debounce of FR-011a. Fed every poll with what the pointer gave (`None`: no new
/// state), it gives the filtered state: pressed or not, and where.
#[derive(Debug, Default, Clone)]
pub struct Debounce {
    raw_down: bool,
    last_state_ms: u64,
    /// The filtered press, and where.
    down: bool,
    at: (u32, u32),
    raw_up_ms: Option<u64>,
    released_ms: Option<u64>,
    /// A press that began too soon after a release: ignored until it ends.
    ignoring: bool,
}

impl Debounce {
    pub fn feed(&mut self, now: u64, sample: Option<(bool, u32, u32)>) -> (bool, u32, u32) {
        match sample {
            Some((pressed, u, v)) => {
                self.last_state_ms = now;
                if pressed {
                    if !self.raw_down && !self.down && self.released_ms.is_some_and(|t| now.saturating_sub(t) < QUIET_MS) {
                        self.ignoring = true;
                    }
                    self.raw_down = true;
                    self.raw_up_ms = None;
                    if !self.ignoring {
                        self.down = true;
                        self.at = (u, v);
                    }
                } else if self.raw_down {
                    self.raw_down = false;
                    self.raw_up_ms = Some(now);
                }
            }
            None => {
                if self.raw_down && now.saturating_sub(self.last_state_ms) >= QUIET_MS {
                    self.raw_down = false;
                    self.raw_up_ms = Some(now);
                }
            }
        }
        if !self.raw_down {
            self.ignoring = false;
            if self.down && self.raw_up_ms.is_some_and(|t| now.saturating_sub(t) >= BOUNCE_MS) {
                self.down = false;
                self.released_ms = Some(now);
            }
        }
        (self.down, self.at.0, self.at.1)
    }
}

#[cfg(test)]
mod tests;
