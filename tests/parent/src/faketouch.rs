// SPDX-License-Identifier: GPL-2.0-or-later
//! A stand-in for the Note Air firmware's I2C and GPIO protocols and the cyttsp5 touch
//! controller behind them (`parent.faketouch=<mode>` in `\parent.cfg`), from the
//! interface description only (`docs/firmware-i2c-gpio.md`).
//!
//! - The GPIO protocol: eight words, revision 1; `configure`, `read pin`, `set pin` record
//!   every call in order; a pin configured as an output reads back its level, an input
//!   reads 0, except the interrupt (67), which reads low while a report waits.
//! - The I2C protocol: six words, revision 0x10000; `open` of instance 3 only, `read`
//!   and `write` of address 0x24 only.
//! - The controller answers only while both power pins (61, 62) are high and its reset
//!   (66) is high, after a reset pulse. After the reset it has the reset sentinel
//!   `00 00` waiting (with nothing waiting a read gives length 2); the bootloader's
//!   launch-app command (its answer, then the application's sentinel), the HID descriptor (register 0x0001)
//!   and `GET_SYSINFO` (to register 0x0004) each queue their answer. A read of register
//!   0x03 gives the first bytes of the waiting frame and takes it when it reads it whole.
//!
//! Modes: `1` all good; `powered` 61 and 62 read high before anything (the controller
//! found powered); `fail-open`, `fail-write`, `nodesc` (no answer to the descriptor),
//! `baddesc` (a descriptor of the wrong version), `slow-boot` (the answer to launch-app
//! 30 reads late, about 150 ms), `nack-first` (the first three writes not taken),
//! `late-app` (the application's sentinel 140 reads, about 700 ms, after the answer,
//! reads and writes not taken until then), `sysinfo-late` (the first `GET_SYSINFO`
//! answered 500 reads, about 2.5 s, late), `sysinfo-second` (the first never answered,
//! the second 150 reads late), `sysinfo-retry` (the first two
//! `GET_SYSINFO` answers empty), `nosysinfo` (never one), `hang-read` (an I2C read that
//! never returns), `fail-gpio` (`set pin` fails).
//!
//! `parent.touchtaps=n:x:y,...` scripts taps in the controller's coordinates, as
//! `parent.taps` does for the stand-in pointer: tap k once the loader has made n
//! refreshes and 60 reads of the input register passed since the last one; five touch
//! reports with the contact down, then one with it lifted off, one per read.
//!
//! After the loader returns, [`report`] prints the counts and the call sequence.
use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::ffi::c_void;
use uefi::boot;
use uefi::{Guid, Status, guid};

const I2C_GUID: Guid = guid!("b27ae8b1-3e10-4d07-ab5c-eb9a6dc6fa8f");
const GPIO_GUID: Guid = guid!("ad9aec18-7bf0-4809-9e96-3012309f3df7");
const ADDR: u32 = 0x24;
const INSTANCE: u32 = 3;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Good,
    Powered,
    FailOpen,
    FailWrite,
    NoDesc,
    BadDesc,
    SysinfoRetry,
    NoSysinfo,
    HangRead,
    FailGpio,
    SlowBoot,
    NackFirst,
    LateApp,
    SysinfoLate,
    SysinfoSecond,
}

impl Mode {
    pub fn parse(w: &str) -> Option<Mode> {
        Some(match w {
            "1" => Mode::Good,
            "powered" => Mode::Powered,
            "fail-open" => Mode::FailOpen,
            "fail-write" => Mode::FailWrite,
            "nodesc" => Mode::NoDesc,
            "baddesc" => Mode::BadDesc,
            "sysinfo-retry" => Mode::SysinfoRetry,
            "nosysinfo" => Mode::NoSysinfo,
            "hang-read" => Mode::HangRead,
            "fail-gpio" => Mode::FailGpio,
            "slow-boot" => Mode::SlowBoot,
            "nack-first" => Mode::NackFirst,
            "late-app" => Mode::LateApp,
            "sysinfo-late" => Mode::SysinfoLate,
            "sysinfo-second" => Mode::SysinfoSecond,
            _ => return None,
        })
    }
}

struct State {
    mode: Mode,
    /// Per pin: Some(level) when an output.
    out: [Option<bool>; 128],
    /// The latched output level per pin (set before configure).
    latch: [bool; 128],
    /// The controller has seen its reset low while powered.
    reset_seen: bool,
    up: bool,
    queue: VecDeque<Vec<u8>>,
    sysinfo_asks: u32,
    seq: Vec<String>,
    counts: [u32; 8],
    open_handles: u32,
    taps: Vec<(u32, u16, u16)>,
    /// Reads of the input register so far: the stand-in's clock (the loader polls about
    /// every 5 ms).
    reads: u32,
    /// Frames due at a read count.
    due: Vec<(u32, Vec<u8>)>,
    /// Reads and writes not taken in this window of read counts.
    busy: (u32, u32),
    nacks: u32,
    tap_next: usize,
    tap_read: u32,
    tap_quiet: u32,
}

static mut ST: Option<State> = None;

fn st() -> &'static mut State {
    // SAFETY: single-threaded UEFI, one caller at a time.
    unsafe { (*core::ptr::addr_of_mut!(ST)).as_mut().expect("faketouch installed") }
}

fn pin_of(cfg: u32) -> usize {
    ((cfg >> 4) & 0x3ff) as usize
}

fn level(s: &State, p: usize) -> bool {
    match s.out[p] {
        Some(l) => l,
        None if p == 67 => s.queue.is_empty() || !s.up,
        None => false,
    }
}

fn powered(s: &State) -> bool {
    level(s, 61) && level(s, 62)
}

/// The controller's state after a pin change.
fn settle(s: &mut State) {
    if !powered(s) {
        s.up = false;
        s.reset_seen = false;
        s.queue.clear();
        return;
    }
    let reset_high = level(s, 66);
    if !reset_high {
        s.reset_seen = true;
        s.up = false;
        s.queue.clear();
    } else if s.reset_seen && !s.up {
        s.up = true;
        s.queue.push_back(alloc::vec![0, 0]);
    }
}

// ---- GPIO ---------------------------------------------------------------------------------

unsafe extern "efiapi" fn gpio_configure(cfg: u32, enable: u32) -> Status {
    let s = st();
    s.counts[0] += 1;
    let p = pin_of(cfg);
    let output = cfg & (1 << 14) != 0;
    s.seq.push(format!("cfg{}{}", p, if output { "o" } else { "i" }));
    if cfg & 0x2000_0000 == 0 || enable != 1 || cfg & !(0x2000_0000 | 0x3ff0 | (1 << 14)) != 0 {
        s.seq.push(String::from("cfg-bad"));
        return Status::UNSUPPORTED;
    }
    s.out[p] = if output { Some(s.latch[p]) } else { None };
    settle(s);
    Status::SUCCESS
}

unsafe extern "efiapi" fn gpio_group(_e: u32, _c: *mut u32, _n: u32) -> Status {
    st().counts[1] += 1;
    Status::UNSUPPORTED
}

unsafe extern "efiapi" fn gpio_read(cfg: u32, out: *mut u32) -> Status {
    let s = st();
    s.counts[2] += 1;
    let p = pin_of(cfg);
    let l = level(s, p);
    s.seq.push(format!("rd{p}={}", l as u8));
    unsafe { *out = l as u32 };
    Status::SUCCESS
}

unsafe extern "efiapi" fn gpio_set(cfg: u32, v: u32) -> Status {
    let s = st();
    s.counts[3] += 1;
    let p = pin_of(cfg);
    s.seq.push(format!("set{p}={}", (v != 0) as u8));
    if s.mode == Mode::FailGpio {
        return Status::UNSUPPORTED;
    }
    s.latch[p] = v != 0;
    if s.out[p].is_some() {
        s.out[p] = Some(v != 0);
    }
    settle(s);
    Status::SUCCESS
}

unsafe extern "efiapi" fn gpio_other(_a: u32, _b: u32) -> Status {
    st().counts[4] += 1;
    Status::UNSUPPORTED
}

// ---- I2C ----------------------------------------------------------------------------------

#[repr(C)]
struct BusCfg {
    khz: u32,
    address: u32,
}

static mut HANDLE_TOKEN: u64 = 0x5a5a;

unsafe extern "efiapi" fn i2c_open(instance: u32, h: *mut *mut c_void) -> u32 {
    let s = st();
    s.counts[5] += 1;
    s.seq.push(format!("open{instance}"));
    if s.mode == Mode::FailOpen {
        return 2;
    }
    if instance != INSTANCE {
        return 1;
    }
    s.open_handles += 1;
    unsafe { *h = core::ptr::addr_of_mut!(HANDLE_TOKEN) as *mut c_void };
    0
}

fn check_handle(h: *mut c_void, cfg: *const BusCfg) -> bool {
    h == core::ptr::addr_of_mut!(HANDLE_TOKEN) as *mut c_void
        && !cfg.is_null()
        && unsafe { (*cfg).address } == ADDR
        && unsafe { (*cfg).khz } == 100
}

unsafe extern "efiapi" fn i2c_read(h: *mut c_void, cfg: *const BusCfg, off: u16, off_len: u16, buf: *mut u8, len: u16, done: *mut u32, timeout: u32) -> u32 {
    let s = st();
    s.counts[6] += 1;
    if s.mode == Mode::HangRead && s.up {
        crate::say("PARENT_FAKETOUCH_HANG");
        loop {
            core::hint::spin_loop();
        }
    }
    if !check_handle(h, cfg) || buf.is_null() || len == 0 || timeout == 0 || off_len != 1 || off != 3 {
        s.seq.push(String::from("rd-bad"));
        return 1;
    }
    if !s.up {
        return 0x8001; // no answer: a NACK
    }
    s.reads += 1;
    if (s.busy.0..s.busy.1).contains(&s.reads) {
        return 0x1100_00c8;
    }
    let now = s.reads;
    let (ready, later): (Vec<_>, Vec<_>) = core::mem::take(&mut s.due).into_iter().partition(|(t, _)| *t <= now);
    s.due = later;
    s.queue.extend(ready.into_iter().map(|(_, f)| f));
    if len == 2 && s.queue.is_empty() {
        next_tap(s);
    }
    if s.queue.is_empty() {
        // Nothing waiting: length 2.
        let out = unsafe { core::slice::from_raw_parts_mut(buf, len as usize) };
        out.fill(0);
        out[0] = 2;
        if !done.is_null() {
            unsafe { *done = len as u32 };
        }
        return 0;
    }
    let out = unsafe { core::slice::from_raw_parts_mut(buf, len as usize) };
    out.fill(0);
    let mut take = false;
    if let Some(f) = s.queue.front() {
        let n = f.len().min(out.len());
        out[..n].copy_from_slice(&f[..n]);
        take = out.len() >= f.len() && f.len() > 2 || f.len() <= 2;
    }
    if take {
        s.queue.pop_front();
    }
    if !done.is_null() {
        unsafe { *done = len as u32 };
    }
    0
}

fn sysinfo() -> Vec<u8> {
    let mut f = alloc::vec![0u8; 51];
    f[0] = 51;
    f[2] = 0x1f;
    f[4] = 0x02;
    // The sensing configuration at 33: electrodes, len_x, len_y, res_x, res_y, max_z, ...
    let sc = 33;
    f[sc + 6..sc + 8].copy_from_slice(&1872u16.to_le_bytes());
    f[sc + 8..sc + 10].copy_from_slice(&1404u16.to_le_bytes());
    f[sc + 10..sc + 12].copy_from_slice(&255u16.to_le_bytes());
    f[sc + 15] = 10;
    f
}

fn descriptor(version: u16) -> Vec<u8> {
    let mut d = alloc::vec![0u8; 32];
    d[0] = 32;
    d[2] = 0xf7; // the application's packet id
    d[4..6].copy_from_slice(&version.to_le_bytes());
    d[8] = 0x02; // report descriptor register
    d[10] = 0x03; // input register
    d[12..14].copy_from_slice(&512u16.to_le_bytes());
    d[14] = 0x04; // output register
    d[18] = 0x05; // command register
    d[22..24].copy_from_slice(&0x04b4u16.to_le_bytes());
    d[24..26].copy_from_slice(&0xc101u16.to_le_bytes());
    d
}

unsafe extern "efiapi" fn i2c_write(h: *mut c_void, cfg: *const BusCfg, off: u16, off_len: u16, buf: *const u8, len: u16, done: *mut u32, timeout: u32) -> u32 {
    let s = st();
    s.counts[7] += 1;
    if !check_handle(h, cfg) || buf.is_null() || len == 0 || timeout == 0 || off_len != 1 {
        s.seq.push(String::from("wr-bad"));
        return 1;
    }
    let data = unsafe { core::slice::from_raw_parts(buf, len as usize) };
    let reg = off | (data[0] as u16) << 8;
    s.seq.push(format!("wr{:04x}:{}", reg, data[1..].iter().map(|b| format!("{b:02x}")).collect::<String>()));
    if s.mode == Mode::FailWrite {
        return 0x8002;
    }
    if s.mode == Mode::NackFirst && s.nacks < 3 {
        s.nacks += 1;
        return 0x1100_00c8;
    }
    if !s.up || (s.busy.0..s.busy.1).contains(&s.reads) {
        return 0x1100_00c8;
    }
    match (reg, &data[1..]) {
        (0x0001, []) => match s.mode {
            Mode::NoDesc => {}
            Mode::BadDesc => s.queue.push_back(descriptor(0x0200)),
            _ => s.queue.push_back(descriptor(0x0100)),
        },
        (0x0004, [0x0b, 0x00, 0x40, 0x00, 0x01, 0x3b, ..]) => {
            // The bootloader's answer, then the application's sentinel (in reads).
            let answer = s.reads + if s.mode == Mode::SlowBoot { 30 } else { 2 };
            let app = answer + if s.mode == Mode::LateApp { 140 } else { 4 };
            s.due.push((answer, alloc::vec![0x0b, 0, 0x30, 0, 0x01, 0, 0, 0, 0, 0, 0x17]));
            s.due.push((app, alloc::vec![0, 0]));
            if s.mode == Mode::LateApp {
                s.busy = (answer + 4, app);
            }
        }
        (0x0004, [0x05, 0x00, 0x2f, 0x00, 0x02]) => {
            s.sysinfo_asks += 1;
            let empty = match s.mode {
                Mode::NoSysinfo => true,
                Mode::SysinfoRetry => s.sysinfo_asks <= 2,
                _ => false,
            };
            match s.mode {
                Mode::SysinfoLate if s.sysinfo_asks == 1 => s.due.push((s.reads + 500, sysinfo())),
                Mode::SysinfoSecond if s.sysinfo_asks == 1 => {}
                Mode::SysinfoSecond => s.due.push((s.reads + 150, sysinfo())),
                _ => s.queue.push_back(if empty { alloc::vec![0, 0] } else { sysinfo() }),
            }
        }
        _ => {}
    }
    if !done.is_null() {
        unsafe { *done = len as u32 };
    }
    0
}

unsafe extern "efiapi" fn i2c_transfer(_h: *mut c_void, _c: *const BusCfg, _d: *const c_void, _n: u16, _w: *mut u32, _r: *mut u32, _t: u32) -> u32 {
    st().seq.push(String::from("transfer"));
    1
}

unsafe extern "efiapi" fn i2c_close(_h: *mut c_void) -> u32 {
    let s = st();
    s.seq.push(String::from("close"));
    s.open_handles = s.open_handles.saturating_sub(1);
    0
}

fn leak<T>(v: T) -> u64 {
    Box::into_raw(Box::new(v)) as u64
}

/// Installs both protocols, each on a handle of its own.
pub fn install(mode: Mode) -> String {
    let mut s = State {
        mode,
        out: [None; 128],
        latch: [false; 128],
        reset_seen: false,
        up: false,
        queue: VecDeque::new(),
        sysinfo_asks: 0,
        seq: Vec::new(),
        counts: [0; 8],
        open_handles: 0,
        taps: Vec::new(),
        reads: 0,
        due: Vec::new(),
        busy: (0, 0),
        nacks: 0,
        tap_next: 0,
        tap_read: 0,
        tap_quiet: 1000,
    };
    if mode == Mode::Powered {
        s.out[61] = Some(true);
        s.out[62] = Some(true);
        s.latch[61] = true;
        s.latch[62] = true;
    }
    unsafe { ST = Some(s) };
    let gpio: [u64; 8] = [
        1,
        gpio_configure as *const () as u64,
        gpio_group as *const () as u64,
        gpio_read as *const () as u64,
        gpio_set as *const () as u64,
        gpio_other as *const () as u64,
        gpio_other as *const () as u64,
        gpio_other as *const () as u64,
    ];
    let i2c: [u64; 6] = [
        0x10000,
        i2c_open as *const () as u64,
        i2c_read as *const () as u64,
        i2c_write as *const () as u64,
        i2c_transfer as *const () as u64,
        i2c_close as *const () as u64,
    ];
    unsafe {
        boot::install_protocol_interface(None, &GPIO_GUID, leak(gpio) as *const c_void).expect("install gpio");
        boot::install_protocol_interface(None, &I2C_GUID, leak(i2c) as *const c_void).expect("install i2c");
    }
    String::from("PARENT_FAKETOUCH installed")
}

pub fn set_taps(words: &str) {
    let s = st();
    for t in words.split(',').filter(|t| !t.is_empty()) {
        let p: Vec<u32> = t.split(':').map(|x| x.parse().expect("parent.touchtaps")).collect();
        assert!(p.len() == 3, "parent.touchtaps: n:x:y");
        s.taps.push((p[0], p[1] as u16, p[2] as u16));
    }
}

/// A touch report with one contact (`down`) or the contact lifting off.
fn touch_report(x: u16, y: u16, down: bool) -> Vec<u8> {
    let mut f = alloc::vec![0u8; 17];
    f[0] = 17;
    f[2] = 0x01;
    f[5] = 1;
    f[8] = if down { 0x20 } else { 0x60 }; // contact 0, event touch / lift-off
    f[9..11].copy_from_slice(&x.to_le_bytes());
    f[11..13].copy_from_slice(&y.to_le_bytes());
    f
}

/// The next scripted report, at a read of the length while nothing waits.
fn next_tap(s: &mut State) {
    let Some(&(after, x, y)) = s.taps.get(s.tap_next) else { return };
    if s.tap_read == 0 {
        s.tap_quiet += 1;
        if crate::fakefw::refresh_count() < after || s.tap_quiet < 60 {
            return;
        }
        crate::say(&format!("PARENT_FAKETOUCH_TAP {} at={x},{y} after_refresh={}", s.tap_next, crate::fakefw::refresh_count()));
    }
    let down = s.tap_read < 5;
    s.queue.push_back(touch_report(x, y, down));
    s.tap_read += 1;
    if !down {
        s.tap_read = 0;
        s.tap_quiet = 0;
        s.tap_next += 1;
    }
}

pub fn report() -> Vec<String> {
    let s = st();
    let c = s.counts;
    alloc::vec![
        format!(
            "PARENT_FAKETOUCH calls configure={} group={} pin_read={} set={} other={} open={} read={} write={} open_now={} powered={}",
            c[0],
            c[1],
            c[2],
            c[3],
            c[4],
            c[5],
            c[6],
            c[7],
            s.open_handles,
            powered(s) as u8
        ),
        format!("PARENT_FAKETOUCH seq {}", s.seq.join(" ")),
    ]
}
