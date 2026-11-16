// SPDX-License-Identifier: GPL-2.0-or-later
use super::*;
use crate::plan::{Log, ReadErr, RecordStore};
use crate::record::{self, SLOT_BYTES};
use crate::sha256_hex;
use alloc::vec;
use core::fmt;
use std::string::String as S;

const BASE: &[(&str, &str)] = &[
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

fn slot(seq: u64, keys: &[(&str, &str)]) -> Vec<u8> {
    let mut lines: Vec<(S, S)> = BASE.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
    for (k, v) in keys {
        match lines.iter_mut().find(|(x, _)| x == k) {
            Some(l) => l.1 = v.to_string(),
            None => lines.push((k.to_string(), v.to_string())),
        }
    }
    let mut t = format!("format=1\nseq={seq}\n");
    for (k, v) in &lines {
        t.push_str(&format!("{k}={v}\n"));
    }
    let mut b = format!("{t}check={}\n", sha256_hex(t.as_bytes())).into_bytes();
    b.resize(SLOT_BYTES, b'\n');
    b
}

/// Both slots the same record (`both`), or only slot a valid.
fn rec(keys: &[(&str, &str)], both: bool) -> InUse {
    let a = slot(1, keys);
    let b = if both { a.clone() } else { vec![0u8; SLOT_BYTES] };
    record::in_use(Some(&a), Some(&b)).unwrap()
}

struct Mem {
    slots: [Vec<u8>; 2],
}
impl RecordStore for Mem {
    fn read_slot(&mut self, i: usize) -> Result<Vec<u8>, ReadErr> {
        Ok(self.slots[i].clone())
    }
    fn write_slot(&mut self, i: usize, b: &[u8]) -> bool {
        self.slots[i] = b.to_vec();
        true
    }
}
impl Log for Mem {
    fn log(&mut self, _: fmt::Arguments) {}
}

const A: &str = "0123456789abcdef";
const B: &str = "fedcba9876543210";

fn ep(r: &InUse) -> EpdPlan {
    plan::epd_plan(Some(r))
}

#[test]
fn the_menu_opens_on_a_request_or_after_an_unmarked_start_only() {
    let r = rec(&[("request", "menu")], true);
    assert_eq!(opens(Some(&r), &ep(&r), false), Some(Why::Request));
    // One valid slot: no request is taken.
    let r = rec(&[("request", "menu")], false);
    assert_eq!(opens(Some(&r), &ep(&r), false), None);
    let r = rec(&[("unmarked", "1")], true);
    assert_eq!(opens(Some(&r), &ep(&r), false), Some(Why::Unmarked));
    let r = rec(&[("unmarked", "2")], false);
    assert_eq!(opens(Some(&r), &ep(&r), false), Some(Why::Unmarked));
    // Nothing asks for it.
    let r = rec(&[], true);
    assert_eq!(opens(Some(&r), &ep(&r), false), None);
    assert_eq!(opens(None, &plan::epd_plan(None), false), None);
    // A draw that may have hung, the sticky key, the parent's recovery pick: never.
    let r = rec(&[("unmarked", "1"), ("drew", "1")], true);
    assert_eq!(opens(Some(&r), &ep(&r), false), None);
    let r = rec(&[("request", "menu"), ("unmarked", "1"), ("drew", "1")], true);
    assert_eq!(opens(Some(&r), &ep(&r), false), None);
    let r = rec(&[("request", "menu"), ("display_failed", "1")], true);
    assert_eq!(opens(Some(&r), &ep(&r), false), None);
    let r = rec(&[("request", "menu")], true);
    assert_eq!(opens(Some(&r), &ep(&r), true), None);
    // drew=1 with unmarked 0 (an older loader's, then a healthy mark): no hang.
    let r = rec(&[("request", "menu"), ("drew", "1")], true);
    assert_eq!(opens(Some(&r), &ep(&r), false), Some(Why::Request));
}

#[test]
fn the_menu_write_counts_marks_and_clears_the_request() {
    let r = rec(&[("request", "menu"), ("unmarked", "2")], true);
    assert_eq!(open_write(&r), vec![("request", S::new()), ("unmarked", "3".into()), ("drew", "1".into())]);
    let r = rec(&[("request", "trial"), ("unmarked", "1")], true);
    assert_eq!(open_write(&r), vec![("unmarked", "2".into()), ("drew", "1".into())]);
}

/// The ordinary pick writes what the start without the menu writes, and answers the same.
#[test]
fn the_ordinary_pick_is_the_start_without_the_menu() {
    let cases: Vec<Vec<(&str, &str)>> = vec![
        vec![],
        vec![("default", A), ("default_rec", "aa")],
        vec![("unmarked", "2"), ("default", A)],
        vec![("unmarked", "3"), ("previous", B), ("previous_rec", "bb")],
        vec![("unmarked", "3")],
        vec![("unmarked", "4"), ("previous", B)],
        vec![("request", "recovery")],
        vec![("request", "trial"), ("trial", B), ("trial_rec", "cc")],
        vec![("request", "trial"), ("unmarked", "1")],
        vec![("request", "menu"), ("unmarked", "1")],
    ];
    for keys in cases {
        for both in [true, false] {
            let r = rec(&keys, both);
            let mut m = Mem { slots: [slot(1, &keys), if both { slot(1, &keys) } else { vec![0; SLOT_BYTES] }] };
            let want = plan::handback(&mut m, Some(r.clone()), false);
            let after = plan::read_record(&mut m).unwrap();
            let d = decide(&r, &Choice::Ordinary);
            assert_eq!(d.answer, want, "{keys:?} both={both}");
            let (got, _) = record::next(&r, &d.changes.iter().map(|(k, v)| (*k, v.as_str())).collect::<Vec<_>>()).unwrap();
            for k in ["unmarked", "started", "started_why", "request"] {
                assert_eq!(got.rec.get(k), after.rec.get(k), "{k} of {keys:?} both={both}");
            }
            assert_eq!(d.counted, after.rec.get("unmarked") != r.rec.get("unmarked"), "{keys:?}");
        }
    }
}

#[test]
fn each_pick_answers_and_writes() {
    let r = rec(&[("default", A), ("default_rec", "aa"), ("previous", B), ("previous_rec", "bb"), ("unmarked", "1")], true);
    let d = decide(&r, &Choice::PreviousKernel);
    assert_eq!(d.answer, Answer::new(Boot::Set { id: B.into(), rec: "bb".into() }, "choice"));
    assert!(d.counted);
    let w = pick_write(&r, &d);
    assert_eq!(
        w,
        vec![("unmarked", "2".into()), ("started", B.into()), ("started_why", "choice".into()), ("drew", S::new())]
    );

    let d = decide(&r, &Choice::Recovery);
    assert_eq!((d.answer.clone(), d.counted), (Answer::new(Boot::Recovery, "choice"), false));
    assert_eq!(pick_write(&r, &d), vec![("unmarked", "1".into()), ("drew", S::new())], "the recovery is not counted");

    let d = decide(&r, &Choice::Deploy("d1".into()));
    assert_eq!(d.answer.boot, Boot::Set { id: A.into(), rec: "aa".into() });
    assert_eq!(d.answer.cmdline_add.as_deref(), Some("glassine.deploy.once=d1"));
    assert_eq!(d.answer.why, "default");
    assert_eq!(
        pick_write(&r, &d),
        vec![("unmarked", "2".into()), ("started", A.into()), ("started_why", "default".into()), ("drew", S::new())]
    );

    // A deployment at the limit with no previous set: the recovery, no once-word.
    let r = rec(&[("unmarked", "3")], true);
    let d = decide(&r, &Choice::Deploy("d1".into()));
    assert_eq!((d.answer.boot.clone(), d.answer.cmdline_add.clone()), (Boot::Recovery, None));
    // The baseline when default is empty.
    let r = rec(&[("request", "menu")], true);
    let d = decide(&r, &Choice::Ordinary);
    assert_eq!(d.answer, Answer::new(Boot::Baseline, "default"));
    assert_eq!(not_written(&Choice::Recovery).boot, Boot::Recovery);
    assert_eq!(not_written(&Choice::Ordinary), Answer::new(Boot::Baseline, "not-written"));
}

#[test]
fn rectangles_merge_and_map_to_aligned_native() {
    let r = |x, y, w, h| Rect { x, y, w, h };
    // Strips of one area become one; far-apart areas stay apart; past four, the box.
    assert_eq!(merge(&[r(0, 0, 100, 64), r(0, 64, 100, 64)], 4), vec![r(0, 0, 100, 128)]);
    assert_eq!(merge(&[r(0, 0, 10, 10), r(0, 500, 10, 10)], 4).len(), 2);
    let many: Vec<Rect> = (0..6).map(|i| r(0, i * 100, 10, 10)).collect();
    assert_eq!(merge(&many, 4), vec![r(0, 0, 10, 510)]);
    assert!(merge(&[r(5, 5, 0, 9)], 4).is_empty());

    let (w, h) = (1872, 1404);
    // The whole portrait panel is the whole native one.
    assert_eq!(native(r(0, 0, 1404, 1872), w, h), Some(r(0, 0, 1872, 1404)));
    // Portrait (u, v) = (10, 20), 30 wide, 40 high: native x = 1872 - 20 - 40 = 1812,
    // rounded down to 1808; x + w = 1852 rounded up to 1856; y = 10, h = 30.
    assert_eq!(native(r(10, 20, 30, 40), w, h), Some(r(1808, 10, 48, 30)));
    // At the bottom of the portrait panel (native x 0) and the right edge.
    assert_eq!(native(r(1400, 1870, 100, 100), w, h), Some(r(0, 1400, 8, 4)));
    assert_eq!(native(r(2000, 0, 10, 10), w, h), None);
    for u in (0..1404).step_by(37) {
        for v in (0..1872).step_by(53) {
            let n = native(r(u, v, 17, 9), w, h).unwrap();
            assert!(n.x % 8 == 0 && n.w % 8 == 0 && n.x + n.w <= w && n.y + n.h <= h, "{n:?}");
            // It covers every native pixel of the portrait rectangle.
            for (pu, pv) in [(u, v), (u + 16, v + 8)] {
                let (pu, pv) = (pu.min(1403), pv.min(1871));
                let (nx, ny) = (w - 1 - pv, pu);
                assert!(nx >= n.x && nx < n.x + n.w && ny >= n.y && ny < n.y + n.h, "{u},{v} {n:?}");
            }
        }
    }
}

#[test]
fn touch_maps_through_the_ranges_and_the_orientation() {
    let (w, h) = (1872, 1404);
    let rg = Range { min_x: 0, max_x: 4095, min_y: 0, max_y: 3071 };
    let id = Orient { swap: false, flip_x: false, flip_y: false };
    // The pointer's origin is the native origin: portrait (0, W - 1).
    assert_eq!(to_portrait(0, 0, rg, id, w, h), Some((0, 1871)));
    assert_eq!(to_portrait(4095, 3071, rg, id, w, h), Some((1403, 0)));
    assert_eq!(to_portrait(9999, 9999, rg, id, w, h), Some((1403, 0)), "clamped");
    // Swapped and flipped.
    let o = Orient { swap: true, flip_x: true, flip_y: false };
    assert_eq!(to_portrait(0, 0, rg, o, w, h), Some((0, 0)));
    // Unusable ranges.
    assert_eq!(to_portrait(1, 1, Range { min_x: 5, max_x: 5, min_y: 0, max_y: 9 }, id, w, h), None);
    // Round trips for every orientation.
    for swap in [false, true] {
        for fx in [false, true] {
            for fy in [false, true] {
                let o = Orient { swap, flip_x: fx, flip_y: fy };
                for (u, v) in [(0, 0), (1403, 1871), (700, 900), (13, 1500)] {
                    let (x, y) = from_portrait(u, v, rg, o, w, h);
                    assert_eq!(to_portrait(x, y, rg, o, w, h), Some((u, v)), "{o:?} {u},{v}");
                }
            }
        }
    }
}

#[test]
fn the_debounce_merges_bounces_ends_silent_presses_and_ignores_double_taps() {
    let mut d = Debounce::default();
    assert_eq!(d.feed(0, None), (false, 0, 0));
    assert_eq!(d.feed(10, Some((true, 5, 6))), (true, 5, 6), "a press counts at once");
    // A 20 ms release is a bounce.
    assert_eq!(d.feed(20, Some((false, 0, 0))).0, true);
    assert_eq!(d.feed(40, Some((true, 7, 8))), (true, 7, 8));
    assert_eq!(d.feed(50, Some((false, 0, 0))).0, true);
    assert_eq!(d.feed(79, None).0, true);
    assert_eq!(d.feed(80, None).0, false, "released after 30 ms");
    // A press 100 ms later is ignored until it ends.
    assert_eq!(d.feed(180, Some((true, 1, 1))).0, false);
    assert_eq!(d.feed(200, Some((false, 0, 0))).0, false);
    // Later presses count; one whose release never comes ends after 300 ms of silence.
    assert_eq!(d.feed(600, Some((true, 9, 9))), (true, 9, 9));
    assert_eq!(d.feed(899, None).0, true);
    assert_eq!(d.feed(900, None).0, true, "silent: released at 900, bounce window next");
    assert_eq!(d.feed(930, None).0, false);
}
