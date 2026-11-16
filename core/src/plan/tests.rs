// SPDX-License-Identifier: GPL-2.0-or-later
//! The start decisions on a fake boot partition laid out as the installer writes it.
use super::*;
use crate::record::SLOT_BYTES;
use alloc::collections::BTreeMap;
use alloc::vec;
use std::string::String as S;

// ---- a fake firmware -------------------------------------------------------------------

#[derive(Default)]
struct Fake {
    files: BTreeMap<S, Vec<u8>>,
    read_only: bool,
    /// Bytes the next read-back of a written slot returns wrong.
    corrupt_readback: bool,
    /// Reads and writes in order: ("read"|"write"|"remove", path).
    ops: Vec<(&'static str, S)>,
    now: u64,
    /// Milliseconds a read of this path takes.
    slow: BTreeMap<S, u64>,
    log: Vec<S>,
}

impl Platform for Fake {
    fn read(&mut self, path: &str, max: u64, deadline: Option<u64>) -> Result<Vec<u8>, ReadErr> {
        self.ops.push(("read", path.into()));
        self.now += self.slow.get(path).copied().unwrap_or(0);
        if deadline.is_some_and(|d| self.now > d) {
            return Err(ReadErr::TimedOut);
        }
        let b = self.files.get(path).ok_or(ReadErr::Missing)?;
        if b.len() as u64 > max {
            return Err(ReadErr::TooLarge(b.len() as u64));
        }
        let mut b = b.clone();
        if self.corrupt_readback && path.contains("loader.") && self.ops.iter().any(|(o, _)| *o == "write") {
            b[0] ^= 1;
        }
        Ok(b)
    }
    fn write_file(&mut self, path: &str, bytes: &[u8]) -> bool {
        self.ops.push(("write", path.into()));
        if self.read_only {
            return false;
        }
        self.files.insert(path.into(), bytes.to_vec());
        true
    }
    fn remove(&mut self, path: &str) -> bool {
        self.ops.push(("remove", path.into()));
        !self.read_only && self.files.remove(path).is_some()
    }
    fn exists(&mut self, path: &str) -> bool {
        self.files.contains_key(path)
    }
    fn dirs(&mut self, path: &str) -> Vec<S> {
        let pre = format!("{path}\\");
        let mut v: Vec<S> = self
            .files
            .keys()
            .filter_map(|k| k.strip_prefix(&pre).and_then(|r| r.split_once('\\')).map(|(d, _)| d.to_string()))
            .collect();
        v.dedup();
        v
    }
    fn now_ms(&mut self) -> u64 {
        self.now
    }
    fn log(&mut self, line: fmt::Arguments) {
        self.log.push(format!("{line}"));
    }
}

#[derive(Default)]
struct Kernel {
    /// (set id, cmdline, initrd length).
    started: Vec<(S, S, Option<usize>)>,
    /// Ids whose kernel "returns" instead of running.
    returns: Vec<S>,
}

impl Starter for Kernel {
    fn start(&mut self, set: &Loaded, cmdline: &str) -> bool {
        self.started.push((set.set.id.clone(), cmdline.into(), set.initrd.as_ref().map(|i| i.len())));
        !self.returns.contains(&set.set.id)
    }
}

// ---- a boot partition as the installer writes it ---------------------------------------

const SYS: &str = "0f3e26a1-5c7b-4a55-9b8d-2c4f6e8a1b3d";
const UD: &str = "6d1a5e2b-8c4f-4b3a-a9e7-1f2d3c4b5a69";

fn checked(body: &str) -> S {
    format!("{body}check={}\n", sha256_hex(body.as_bytes()))
}

struct Set {
    id: S,
    rec_sha: S,
}

/// Writes a set as `pc_boot::KernelSet` does: id from the sums and the base, the
/// identities and `glassine.img=` on the command line.
fn put_set(f: &mut Fake, dir: &str, kernel: &[u8], initrd: Option<&[u8]>, base: &str) -> Set {
    let ks = sha256_hex(kernel);
    let is = initrd.map(sha256_hex);
    let id = sha256_hex(format!("{ks}\n{}\n{base}\n", is.as_deref().unwrap_or("")).as_bytes())[..16].to_string();
    let mut words: Vec<S> = if base.is_empty() { vec![] } else { vec![base.into()] };
    words.push(format!("glassine.system=PARTUUID={SYS}"));
    words.push(format!("glassine.userdata=PARTUUID={UD}"));
    words.push(format!("glassine.img={id}"));
    let (isha, isize) = match (initrd, &is) {
        (Some(i), Some(s)) => (s.clone(), i.len().to_string()),
        _ => (S::new(), S::new()),
    };
    let rec = checked(&format!(
        "format=1\nid={id}\nrelease=6.18.63-0-lts\nkernel_sha256={ks}\nkernel_size={}\ninitrd_sha256={isha}\ninitrd_size={isize}\ncmdline={}\ninitramfs=\n",
        kernel.len(),
        words.join(" ")
    ));
    let d = format!("{dir}\\{id}");
    f.files.insert(format!("{d}\\vmlinuz"), kernel.to_vec());
    if let Some(i) = initrd {
        f.files.insert(format!("{d}\\initrd"), i.to_vec());
    }
    let rec_sha = sha256_hex(rec.as_bytes());
    f.files.insert(format!("{d}\\set.rec"), rec.into_bytes());
    Set { id, rec_sha }
}

struct Part {
    f: Fake,
    default: Set,
    previous: Set,
    trial: Set,
    recovery: Set,
}

/// The installer's partition, plus a previous and a trial set; `keys` changes the
/// record, written to both slots with `seq=1`.
fn partition(keys: &[(&str, &str)]) -> Part {
    let mut f = Fake::default();
    let default = put_set(&mut f, SETS, b"kernel-default", Some(b"initrd-default"), "console=ttyS0 quiet");
    let previous = put_set(&mut f, SETS, b"kernel-previous", Some(b"initrd-previous"), "console=ttyS0 quiet");
    let trial = put_set(&mut f, SETS, b"kernel-trial", Some(b"initrd-trial"), "console=ttyS0 quiet");
    let recovery = put_set(&mut f, RECOVERY, b"kernel-recovery", None, "quiet");
    let mut pairs: Vec<(S, S)> = [
        ("format", "1"),
        ("seq", "1"),
        ("default", default.id.as_str()),
        ("default_rec", default.rec_sha.as_str()),
        ("previous", previous.id.as_str()),
        ("previous_rec", previous.rec_sha.as_str()),
        ("trial", ""),
        ("trial_rec", ""),
        ("recovery", recovery.id.as_str()),
        ("recovery_rec", recovery.rec_sha.as_str()),
        ("request", ""),
        ("unmarked", "0"),
        ("started", ""),
        ("started_why", ""),
        ("deploys", "0"),
    ]
    .iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect();
    for (k, v) in keys {
        let v = match *v {
            "@trial" => trial.id.clone(),
            "@trial_rec" => trial.rec_sha.clone(),
            v => v.to_string(),
        };
        match pairs.iter_mut().find(|(x, _)| x == k) {
            Some(p) => p.1 = v,
            None => pairs.push((k.to_string(), v)),
        }
    }
    let body: S = pairs.iter().map(|(k, v)| format!("{k}={v}\n")).collect();
    let mut slot = checked(&body).into_bytes();
    slot.resize(SLOT_BYTES, b'\n');
    f.files.insert(SLOTS[0].into(), slot.clone());
    f.files.insert(SLOTS[1].into(), slot);
    Part { f, default, previous, trial, recovery }
}

fn boot(f: &mut Fake, choice: Choice) -> Kernel {
    let mut k = Kernel::default();
    boot_with(f, &mut k, choice);
    k
}

fn boot_with(f: &mut Fake, k: &mut Kernel, choice: Choice) -> bool {
    f.ops.clear();
    let r = read_record(f);
    start(f, k, r, choice)
}

fn rec(f: &mut Fake) -> InUse {
    let a = f.files.get(SLOTS[0]).cloned();
    let b = f.files.get(SLOTS[1]).cloned();
    record::in_use(a.as_deref(), b.as_deref()).unwrap()
}

fn started(k: &Kernel) -> &str {
    &k.started.last().unwrap().0
}

// ---- the cases -------------------------------------------------------------------------

#[test]
fn an_ordinary_start_counts_and_starts_the_default_set() {
    let mut p = partition(&[]);
    let k = boot(&mut p.f, Choice::Ordinary);
    assert_eq!(k.started.len(), 1);
    let (id, cmd, initrd) = &k.started[0];
    assert_eq!(id, &p.default.id);
    assert_eq!(cmd, &format!("console=ttyS0 quiet glassine.system=PARTUUID={SYS} glassine.userdata=PARTUUID={UD} glassine.img={id}"));
    assert_eq!(*initrd, Some(14));
    let r = rec(&mut p.f);
    assert_eq!((r.slot, r.seq), (1, 2), "slot b written, seq raised");
    assert_eq!(r.rec.get("unmarked"), "1");
    assert_eq!(r.rec.get("started"), p.default.id);
    assert_eq!(r.rec.get("started_why"), "default");
    // The write came before the kernel's files were read? No: after the check, before
    // the start. Writes happen only to slot b.
    let writes: Vec<&S> = p.f.ops.iter().filter(|(o, _)| *o == "write").map(|(_, x)| x).collect();
    assert_eq!(writes, [SLOTS[1]]);
    assert_eq!(wait_ms(Some(&r)), WAIT_UNMARKED_MS);
}

#[test]
fn three_unmarked_starts_then_the_previous_set_then_the_recovery() {
    let mut p = partition(&[]);
    for n in 1..=3 {
        let k = boot(&mut p.f, Choice::Ordinary);
        assert_eq!(started(&k), p.default.id);
        assert_eq!(rec(&mut p.f).rec.get("unmarked"), n.to_string());
    }
    let k = boot(&mut p.f, Choice::Ordinary);
    assert_eq!(started(&k), p.previous.id, "at the limit");
    let r = rec(&mut p.f);
    assert_eq!((r.rec.get("unmarked"), r.rec.get("started_why")), ("4", "limit"));
    assert_eq!(r.rec.get("started"), p.previous.id);
    let before = p.f.files.clone();
    let k = boot(&mut p.f, Choice::Ordinary);
    assert_eq!(started(&k), p.recovery.id, "past the limit");
    assert_eq!(p.f.files, before, "a recovery start writes nothing");
    // The healthy mark (unmarked=0, by the running system) starts a new round.
    let cur = rec(&mut p.f);
    let (n, b) = record::next(&cur, &[("unmarked", "0")]).unwrap();
    p.f.files.insert(SLOTS[n.slot].into(), b);
    assert_eq!(started(&boot(&mut p.f, Choice::Ordinary)), p.default.id);
}

#[test]
fn at_the_limit_with_no_previous_set_the_recovery() {
    let mut p = partition(&[("unmarked", "3"), ("previous", ""), ("previous_rec", "")]);
    assert_eq!(started(&boot(&mut p.f, Choice::Ordinary)), p.recovery.id);
    let mut p = partition(&[("unmarked", "3"), ("previous_rec", "0000")]);
    assert_eq!(started(&boot(&mut p.f, Choice::Ordinary)), p.recovery.id, "a previous set that doesn't check");
}

#[test]
fn a_trial_request_is_cleared_before_the_trial_set_is_read() {
    let mut p = partition(&[("trial", "@trial"), ("trial_rec", "@trial_rec"), ("request", "trial"), ("unmarked", "1")]);
    let k = boot(&mut p.f, Choice::Ordinary);
    assert_eq!(started(&k), p.trial.id);
    let r = rec(&mut p.f);
    assert_eq!(r.rec.get("request"), "");
    assert_eq!(r.rec.get("started"), p.trial.id);
    assert_eq!(r.rec.get("started_why"), "request");
    assert_eq!(r.rec.get("unmarked"), "1", "a trial start is not counted");
    assert_eq!(r.rec.get("trial"), p.trial.id, "the trial stays named");
    let w = p.f.ops.iter().position(|(o, _)| *o == "write").unwrap();
    let first_trial_read = p.f.ops.iter().position(|(_, x)| x.contains(&p.trial.id)).unwrap();
    assert!(w < first_trial_read, "{:?}", p.f.ops);
    // The next start is the default set.
    assert_eq!(started(&boot(&mut p.f, Choice::Ordinary)), p.default.id);
}

#[test]
fn a_trial_that_fails_its_check_falls_to_the_default_set_counted_once() {
    let mut p = partition(&[("trial", "@trial"), ("trial_rec", "@trial_rec"), ("request", "trial")]);
    let tk = format!("{SETS}\\{}\\vmlinuz", p.trial.id);
    p.f.files.get_mut(&tk).unwrap()[0] ^= 1;
    let k = boot(&mut p.f, Choice::Ordinary);
    assert_eq!(k.started.len(), 1);
    assert_eq!(started(&k), p.default.id);
    let r = rec(&mut p.f);
    assert_eq!((r.seq, r.rec.get("request"), r.rec.get("unmarked"), r.rec.get("started_why")), (3, "", "1", "default"));
}

#[test]
fn a_recovery_request_is_cleared_and_not_counted() {
    let mut p = partition(&[("request", "recovery"), ("unmarked", "2")]);
    let k = boot(&mut p.f, Choice::Ordinary);
    assert_eq!(started(&k), p.recovery.id);
    assert_eq!(k.started[0].2, None, "the recovery has no initrd");
    let r = rec(&mut p.f);
    assert_eq!((r.rec.get("request"), r.rec.get("unmarked"), r.seq), ("", "2", 2));
}

#[test]
fn with_one_valid_slot_no_request_is_taken_and_the_write_drops_it() {
    let mut p = partition(&[("request", "trial"), ("trial", "@trial"), ("trial_rec", "@trial_rec")]);
    p.f.files.get_mut(SLOTS[1]).unwrap()[5] ^= 1; // slot b damaged
    let k = boot(&mut p.f, Choice::Ordinary);
    assert_eq!(started(&k), p.default.id);
    let r = rec(&mut p.f);
    assert_eq!((r.slot, r.seq, r.rec.get("request"), r.rec.get("unmarked")), (1, 2, "", "1"));
    assert!(p.f.log.iter().any(|l| l.contains("not taken: only one slot")));
}

#[test]
fn a_damaged_slot_b_with_a_higher_seq_leaves_slot_a() {
    let mut p = partition(&[("unmarked", "2")]);
    let cur = rec(&mut p.f);
    let (_, mut b) = record::next(&cur, &[("unmarked", "0")]).unwrap();
    b[30] ^= 0x20; // a cut or a flipped bit: seq 2 in b, but b fails its check
    p.f.files.insert(SLOTS[1].into(), b);
    let k = boot(&mut p.f, Choice::Ordinary);
    assert_eq!(started(&k), p.default.id);
    let r = rec(&mut p.f);
    assert_eq!((r.slot, r.seq, r.rec.get("unmarked")), (1, 2, "3"), "counted from slot a's 2, written over b");
}

#[test]
fn a_kernel_whose_sum_is_wrong_falls_back_in_order() {
    let mut p = partition(&[]);
    let dk = format!("{SETS}\\{}\\vmlinuz", p.default.id);
    p.f.files.get_mut(&dk).unwrap()[0] ^= 1;
    let k = boot(&mut p.f, Choice::Ordinary);
    assert_eq!(started(&k), p.previous.id);
    let r = rec(&mut p.f);
    assert_eq!((r.rec.get("started_why"), r.rec.get("unmarked")), ("limit", "1"));
    // The previous one too: the recovery, by the record.
    let pk = format!("{SETS}\\{}\\initrd", p.previous.id);
    p.f.files.get_mut(&pk).unwrap().push(0);
    assert_eq!(started(&boot(&mut p.f, Choice::Ordinary)), p.recovery.id);
    // The named recovery broken: another recovery directory, highest name first.
    let rk = format!("{RECOVERY}\\{}\\vmlinuz", p.recovery.id);
    p.f.files.get_mut(&rk).unwrap()[0] ^= 1;
    let r1 = put_set(&mut p.f, RECOVERY, b"kernel-rec-1", Some(b"i1"), "quiet");
    let r2 = put_set(&mut p.f, RECOVERY, b"kernel-rec-2", None, "quiet a");
    let want = if r1.id > r2.id { &r1.id } else { &r2.id };
    assert_eq!(started(&boot(&mut p.f, Choice::Ordinary)), want);
    // Nothing that checks: nothing starts, the caller shows the message.
    for id in [r1.id.clone(), r2.id.clone()] {
        p.f.files.remove(&format!("{RECOVERY}\\{id}\\set.rec"));
    }
    let mut k = Kernel::default();
    assert!(!boot_with(&mut p.f, &mut k, Choice::Ordinary));
    assert!(k.started.is_empty());
    assert!(p.f.log.iter().any(|l| l == "NOTHING_STARTED"));
}

#[test]
fn set_rec_checks_name_and_record() {
    let mut p = partition(&[]);
    // set.rec differs from what the loader record names.
    let sr = format!("{SETS}\\{}\\set.rec", p.default.id);
    let t = S::from_utf8(p.f.files[&sr].clone()).unwrap();
    let body = &t[..t.rfind("check=").unwrap()];
    p.f.files.insert(sr.clone(), checked(&body.replace("6.18.63", "6.18.64")).into_bytes());
    assert_eq!(started(&boot(&mut p.f, Choice::Ordinary)), p.previous.id);
    // A set directory the record doesn't name is never started.
    let mut p = partition(&[("default", "0123456789abcdef")]);
    assert_eq!(started(&boot(&mut p.f, Choice::Ordinary)), p.previous.id);
}

#[test]
fn a_loader_that_cant_write_starts_the_default_set_uncounted() {
    for keys in [&[][..], &[("unmarked", "3")][..], &[("unmarked", "7")][..], &[("request", "trial"), ("trial", "@trial"), ("trial_rec", "@trial_rec")][..], &[("request", "recovery")][..]] {
        let mut p = partition(keys);
        p.f.read_only = true;
        let before = p.f.files.clone();
        let k = boot(&mut p.f, Choice::Ordinary);
        let s = started(&k);
        if keys.iter().any(|(k, v)| *k == "unmarked" && *v == "7") {
            assert_eq!(s, p.recovery.id, "past the limit: the recovery, which needs no write");
        } else {
            assert_eq!(s, p.default.id, "{keys:?}");
        }
        assert_eq!(p.f.files, before);
    }
    // A read-back that differs is a write that didn't happen.
    let mut p = partition(&[("request", "trial"), ("trial", "@trial"), ("trial_rec", "@trial_rec")]);
    p.f.corrupt_readback = true;
    assert_eq!(started(&boot(&mut p.f, Choice::Ordinary)), p.default.id);
}

#[test]
fn a_deployment_pick_adds_only_the_request_and_writes_the_same() {
    let mut a = partition(&[("unmarked", "1")]);
    let mut b = partition(&[("unmarked", "1")]);
    let ka = boot(&mut a.f, Choice::Ordinary);
    let kb = boot(&mut b.f, Choice::Deploy("d20261101-a".into()));
    assert_eq!(kb.started[0].1, format!("{} glassine.deploy.once=d20261101-a", ka.started[0].1));
    assert_eq!(a.f.files, b.f.files, "nothing written for the pick");
}

#[test]
fn the_previous_kernel_and_the_recovery_by_choice() {
    let mut p = partition(&[("request", "trial"), ("trial", "@trial"), ("trial_rec", "@trial_rec")]);
    let k = boot(&mut p.f, Choice::PreviousKernel);
    assert_eq!(started(&k), p.previous.id);
    let r = rec(&mut p.f);
    assert_eq!((r.rec.get("started_why"), r.rec.get("unmarked"), r.rec.get("request")), ("choice", "1", "trial"), "a request is left");
    let before = p.f.files.clone();
    assert_eq!(started(&boot(&mut p.f, Choice::Recovery)), p.recovery.id);
    assert_eq!(p.f.files, before, "the recovery by choice writes nothing");
    let mut p = partition(&[("request", "recovery")]);
    assert_eq!(started(&boot(&mut p.f, Choice::Recovery)), p.recovery.id);
    assert_eq!(rec(&mut p.f).rec.get("request"), "", "but clears request=recovery");
}

#[test]
fn with_no_record_the_recovery_is_found_by_itself() {
    let mut p = partition(&[]);
    p.f.files.remove(SLOTS[0]);
    p.f.files.insert(SLOTS[1].into(), vec![b'\n'; SLOT_BYTES]);
    let before = p.f.files.clone();
    let k = boot(&mut p.f, Choice::Ordinary);
    assert_eq!(started(&k), p.recovery.id);
    assert_eq!(p.f.files, before);
}

#[test]
fn a_kernel_that_returns_gives_the_next_choice_without_a_second_count() {
    let mut p = partition(&[]);
    let mut k = Kernel { returns: vec![p.default.id.clone()], ..Default::default() };
    assert!(boot_with(&mut p.f, &mut k, Choice::Ordinary));
    assert_eq!(k.started.iter().map(|x| x.0.as_str()).collect::<Vec<_>>(), [p.default.id.as_str(), p.previous.id.as_str()]);
    let r = rec(&mut p.f);
    assert_eq!((r.seq, r.rec.get("unmarked")), (2, "1"));
}

// ---- the splash ------------------------------------------------------------------------

fn pic(mode: (u32, u32), c: u32) -> Vec<u8> {
    let n = (mode.0 * mode.1) as usize;
    let px: Vec<u8> = (0..n).flat_map(|_| c.to_le_bytes()).collect();
    gspl::encode(0, mode.0, mode.1, mode, 0x102030, 0, &px)
}

fn colour(k: &Pick) -> (&'static str, u32) {
    match k {
        Pick::Override(p) => ("override", p.px[0]),
        Pick::Splash(p, n) => (n, p.px[0]),
        Pick::LeaveScreen => ("leave", 0),
        Pick::Nothing => ("nothing", 0),
    }
}

#[test]
fn the_override_is_removed_before_it_is_drawn_and_used_once() {
    let mut f = Fake::default();
    f.files.insert(OVERRIDE.into(), pic((4, 2), 1));
    f.files.insert(SPLASH.into(), pic((4, 2), 2));
    f.files.insert(DEFAULT_SPLASH.into(), pic((8, 8), 3));
    let k = pick_picture(&mut f, &[(4, 2)]);
    assert_eq!(colour(&k), ("override", 1));
    assert!(!f.files.contains_key(OVERRIDE));
    assert_eq!(f.ops.last().unwrap(), &("remove", OVERRIDE.into()), "removed before anything is drawn");
    assert_eq!(colour(&pick_picture(&mut f, &[(4, 2)])), ("splash", 2));
}

#[test]
fn an_override_that_cant_be_removed_or_doesnt_fit_is_not_drawn() {
    let mut f = Fake::default();
    f.files.insert(OVERRIDE.into(), pic((4, 2), 1));
    f.files.insert(SPLASH.into(), pic((4, 2), 2));
    f.read_only = true;
    assert_eq!(colour(&pick_picture(&mut f, &[(4, 2)])), ("splash", 2));
    assert!(f.files.contains_key(OVERRIDE));
    f.read_only = false;
    assert_eq!(colour(&pick_picture(&mut f, &[(8, 2)])), ("splash", 2), "another mode");
    assert!(!f.files.contains_key(OVERRIDE), "used up all the same");
    f.files.insert(OVERRIDE.into(), pic((4, 2), 1));
    assert_eq!(colour(&pick_picture(&mut f, &[(4, 2), (4, 2)])), ("splash", 2), "two displays");
    assert!(!f.files.contains_key(OVERRIDE));
    f.files.insert(OVERRIDE.into(), gspl::encode(1, 0, 0, (0, 0), 0, 0, &[]));
    assert_eq!(colour(&pick_picture(&mut f, &[(4, 2)])), ("leave", 0));
    f.files.insert(OVERRIDE.into(), b"junk".to_vec());
    assert_eq!(colour(&pick_picture(&mut f, &[(4, 2)])), ("splash", 2));
    assert!(!f.files.contains_key(OVERRIDE), "a damaged override is used up too");
    f.files.insert(OVERRIDE.into(), pic((4, 2), 1));
    assert_eq!(colour(&pick_picture(&mut f, &[])), ("nothing", 0), "no display");
    assert!(!f.files.contains_key(OVERRIDE));
}

#[test]
fn a_bad_or_slow_splash_falls_to_the_default() {
    let mut f = Fake::default();
    let mut bad = pic((4, 2), 2);
    bad[70] ^= 1;
    f.files.insert(SPLASH.into(), bad);
    f.files.insert(DEFAULT_SPLASH.into(), pic((8, 8), 3));
    assert_eq!(colour(&pick_picture(&mut f, &[(4, 2)])), ("default", 3));
    f.files.insert(SPLASH.into(), pic((4, 2), 2));
    f.slow.insert(SPLASH.into(), IMAGE_MS + 1);
    assert_eq!(colour(&pick_picture(&mut f, &[(4, 2)])), ("default", 3));
    f.files.insert(SPLASH.into(), gspl::encode(1, 0, 0, (0, 0), 0, 0, &[]));
    assert_eq!(colour(&pick_picture(&mut f, &[(4, 2)])), ("default", 3), "leave-screen is no splash");
    f.files.remove(DEFAULT_SPLASH);
    assert_eq!(colour(&pick_picture(&mut f, &[(4, 2)])), ("nothing", 0));
}

#[test]
fn epd_plan_draws_counted_after_healthy_and_sticks_after_a_drawn_unmarked_start() {
    let rec = |lines: &str| {
        let body = format!("format=1\nseq=1\n{lines}");
        let t = format!("{body}check={}\n", crate::sha256_hex(body.as_bytes()));
        InUse { rec: record::parse(t.as_bytes()).unwrap(), slot: 0, seq: 1, both_valid: true }
    };
    let a = |why| Answer::new(Boot::Baseline, why);
    let p = epd_plan(Some(&rec("unmarked=0\n")));
    assert!(p.draw && p.counted == vec![("drew", S::from("1"))] && p.any.is_empty(), "{p:?}");
    assert_eq!(epd_draws(&p, &a("default")), Ok(()));
    for why in ["not-written", "request", "choice", "limit", "no-record"] {
        assert!(epd_draws(&p, &a(why)).is_err(), "{why} isn't counted");
    }
    // unmarked, the start before didn't draw: no draw, nothing sticky
    let p = epd_plan(Some(&rec("unmarked=1\n")));
    assert!(!p.draw && p.any.is_empty() && epd_draws(&p, &a("default")).is_err(), "{p:?}");
    // unmarked, the start before drew: display_failed set, drew cleared
    let p = epd_plan(Some(&rec("unmarked=1\ndrew=1\n")));
    assert_eq!(p.any, vec![("display_failed", S::from("1")), ("drew", S::new())]);
    // display_failed: no draw even after a healthy mark
    let p = epd_plan(Some(&rec("unmarked=0\ndisplay_failed=1\n")));
    assert!(!p.draw && epd_draws(&p, &a("default")).is_err(), "{p:?}");
    assert!(epd_plan(None).skip.is_some());
}

#[test]
fn epd_bound_mode_or_splash_buffer_info() {
    // The Note Air's shape: GetFrameBuffer 1872x1404, a Mode of 325x1423x4 bytes, the
    // variable version 1 with a 0x02300000-byte region at the same base.
    let fb = 0x9d40_0000_u64;
    let small = Some((fb, 1_849_900));
    let var = |v: u32, base: u32, size: u32| {
        let mut b = alloc::vec::Vec::new();
        for x in [v, base, size] {
            b.extend_from_slice(&x.to_le_bytes());
        }
        b
    };
    let good = var(1, fb as u32, 0x0230_0000);
    // The probe's hex of the device's variable.
    assert_eq!(crate::hex(&good), "010000000000409d00003002");
    assert_eq!(splash_buffer_region(&good), Some((fb, 0x0230_0000)));
    assert_eq!(epd_bound(fb, 1872, 1404, small, Some(&good)), Ok(EpdBound::SplashBufferInfo));
    // A Mode big enough wins, the variable unread or not.
    assert_eq!(epd_bound(fb, 1872, 1404, Some((fb, 1872 * 1404 * 2)), None), Ok(EpdBound::Mode));
    assert_eq!(epd_bound(fb, 1872, 1404, Some((fb, 1872 * 1404 * 2)), Some(&good)), Ok(EpdBound::Mode));
    // No region: the variable missing, of another version or size.
    assert!(epd_bound(fb, 1872, 1404, small, None).is_err());
    assert!(epd_bound(fb, 1872, 1404, small, Some(&var(2, fb as u32, 0x0230_0000))).is_err());
    assert!(epd_bound(fb, 1872, 1404, small, Some(&good[..8])).is_err());
    let mut long = good.clone();
    long.push(0);
    assert!(epd_bound(fb, 1872, 1404, small, Some(&long)).is_err());
    assert_eq!(splash_buffer_region(&[]), None);
    // A well-formed region that doesn't hold the picture: too small, or elsewhere.
    assert!(epd_bound(fb, 1872, 1404, small, Some(&var(1, fb as u32, 5_256_575))).is_err());
    assert_eq!(epd_bound(fb, 1872, 1404, small, Some(&var(1, fb as u32, 5_256_576))), Ok(EpdBound::SplashBufferInfo));
    assert!(epd_bound(fb, 1872, 1404, small, Some(&var(1, fb as u32 + 8, 0x0230_0000))).is_err());
    // No Mode and an overflowing region.
    assert!(epd_bound(fb, 1872, 1404, None, Some(&var(1, 0xffff_ff00, 0xffff_ffff))).is_err());
    assert!(epd_bound(u64::MAX - 4, 8, 1, Some((0, u64::MAX)), None).is_err());
}
