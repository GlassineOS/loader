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
//!   0x03 gives the first bytes of the waiting frame and takes it when it reads it whole;
//!   a read with the two-byte offset 0x0002 gives the report descriptor.
//!
//! Modes: `1` all good; `powered` 61 and 62 read high before anything (the controller
//! found powered); `fail-open`, `fail-write`, `nodesc` (no answer to the descriptor),
//! `baddesc` (a descriptor of the wrong version), `slow-boot` (the answer to launch-app
//! 150 ms late), `nack-first` (the first three writes not taken),
//! `late-app` (the application's sentinel 700 ms after the answer,
//! reads and writes not taken until then), `sysinfo-late` (the first `GET_SYSINFO`
//! answered 2.5 s late), `sysinfo-second` (the first never answered, the second
//! 770 ms late), `early-sentinel` (a size-0 frame 15 ms after launch-app, too
//! early to be the application's, then the application's sentinel at 85 ms),
//! `linux-trace` (Linux's own probe on the device: no answer to launch-app, the
//! application's five sentinels 80 ms after it; before them the descriptor says
//! bootloader, 0xFF, and a `GET_SYSINFO` is lost; after them `GET_SYSINFO` answered
//! 745 ms later), `irq-stuck` (the interrupt never asserted), `rail-off` (pm660l LDO3 off: the
//! controller answers but senses no taps until it is on), `sysinfo-retry` (the first two
//! `GET_SYSINFO` answers empty), `nosysinfo` (never one), `hang-read` (an I2C read that
//! never returns), `fail-gpio` (`set pin` fails).
//!
//! `parent.touchtaps=n:x:y,...` scripts taps in the controller's coordinates, as
//! `parent.taps` does for the stand-in pointer: tap k once the loader has made n
//! refreshes and 60 reads of the input register passed since the last one; five touch
//! reports with the contact down, then one with it lifted off, one per read.
//!
//! - The PMIC regulator protocol: eight words, revision 0x10005; PMIC 0 LDO11 (1.8 V)
//!   on, PMIC 1 LDO3 (3.3 V, its level in µV) on except in `rail-off`; any other
//!   regulator an invalid parameter. Every call recorded (`vreg ...`).
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
const VREG_GUID: Guid = guid!("22d38d3d-e8b6-4f8f-9c26-bceb07d6cb68");
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
    EarlySentinel,
    LinuxTrace,
    IrqStuck,
    RailOff,
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
            "early-sentinel" => Mode::EarlySentinel,
            "linux-trace" => Mode::LinuxTrace,
            "irq-stuck" => Mode::IrqStuck,
            "rail-off" => Mode::RailOff,
            _ => return None,
        })
    }
}

struct State {
    mode: Mode,
    /// When the application's sentinels come (after launch-app).
    app_at: Option<u64>,
    /// The regulators: (pmic, regulator id) → (on, mV); the calls made.
    rails: Vec<((u32, u32), (bool, u32))>,
    vreg_seq: Vec<String>,
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
    /// Reads of the input register with nothing waiting.
    reads_high: u32,
    /// Reads and writes not taken in this window of read counts.
    busy: (u64, u64),
    nacks: u32,
    /// Frames due at a time in ms (the GET_SYSINFO modes: a real clock, as the loader's
    /// waits are timed, not counted).
    due_ms: Vec<(u64, Vec<u8>)>,
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
        None if p == 67 => s.mode == Mode::IrqStuck || s.queue.is_empty() || !s.up,
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
    if p == 67 {
        due(s);
    }
    let l = level(s, p);
    // The interrupt's polls during the waits are counted, not listed.
    if !(p == 67 && s.open_handles > 0) {
        s.seq.push(format!("rd{p}={}", l as u8));
    }
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
    if off_len == 2 && off == 0x0200 && check_handle(h, cfg) && !buf.is_null() && s.up {
        // The report descriptor (register 0x0002, low byte first on the wire).
        let out = unsafe { core::slice::from_raw_parts_mut(buf, len as usize) };
        out.fill(0);
        out[..4].copy_from_slice(&[0x05, 0x0d, 0x09, 0x04]);
        s.seq.push(format!("rd16:0002:{len}"));
        if !done.is_null() {
            unsafe { *done = len as u32 };
        }
        return 0;
    }
    if !check_handle(h, cfg) || buf.is_null() || len == 0 || timeout == 0 || off_len != 1 || off != 3 {
        s.seq.push(String::from("rd-bad"));
        return 1;
    }
    if !s.up {
        return 0x8001; // no answer: a NACK
    }
    s.reads += 1;
    if (s.busy.0..s.busy.1).contains(&now_ms()) {
        return 0x1100_00c8;
    }
    due(s);
    if len == 2 && s.queue.is_empty() && sensing(s) {
        next_tap(s);
    }
    if s.queue.is_empty() {
        s.reads_high += 1;
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

fn app_started(s: &State) -> bool {
    s.app_at.is_some_and(|t| now_ms() >= t)
}

/// The panel senses only with pm660l LDO3 on.
fn sensing(s: &State) -> bool {
    s.rails.iter().any(|r| r.0 == (1, 2) && r.1.0)
}

fn rail(s: &mut State, pmic: u32, id: u32) -> Option<&mut (bool, u32)> {
    s.rails.iter_mut().find(|r| r.0 == (pmic, id)).map(|r| &mut r.1)
}

unsafe extern "efiapi" fn vreg_control(pmic: u32, id: u32, on: u8) -> Status {
    let s = st();
    s.vreg_seq.push(format!("ctl{pmic}.{id}={on}"));
    match rail(s, pmic, id) {
        Some(r) => {
            r.0 = on != 0;
            Status::SUCCESS
        }
        None => Status::INVALID_PARAMETER,
    }
}

unsafe extern "efiapi" fn vreg_set_level(pmic: u32, id: u32, mv: u32) -> Status {
    let s = st();
    s.vreg_seq.push(format!("set{pmic}.{id}={mv}"));
    match rail(s, pmic, id) {
        Some(r) => {
            r.1 = mv;
            Status::SUCCESS
        }
        None => Status::INVALID_PARAMETER,
    }
}

unsafe extern "efiapi" fn vreg_get_level(pmic: u32, id: u32, out: *mut u32) -> Status {
    let s = st();
    s.vreg_seq.push(format!("lvl{pmic}.{id}"));
    match rail(s, pmic, id) {
        Some(r) => {
            unsafe { *out = if (pmic, id) == (1, 2) { r.1 * 1000 } else { r.1 } };
            Status::SUCCESS
        }
        None => Status::INVALID_PARAMETER,
    }
}

unsafe extern "efiapi" fn vreg_get_status(pmic: u32, id: u32, out: *mut u32) -> Status {
    let s = st();
    s.vreg_seq.push(format!("st{pmic}.{id}"));
    match rail(s, pmic, id) {
        Some(r) => {
            // pull-down, mode, pin control, software enable, then the "ok" byte.
            let on = r.0 as u32;
            unsafe {
                for (i, v) in [0, 2, 0, on, on].into_iter().enumerate() {
                    *out.add(i) = v;
                }
            }
            Status::SUCCESS
        }
        None => Status::INVALID_PARAMETER,
    }
}

unsafe extern "efiapi" fn vreg_other() -> Status {
    st().vreg_seq.push(String::from("other"));
    Status::UNSUPPORTED
}

/// The frames whose time has come, waiting.
fn due(s: &mut State) {
    let t = now_ms();
    let (ready, later): (Vec<_>, Vec<_>) = core::mem::take(&mut s.due_ms).into_iter().partition(|(d, _)| *d <= t);
    s.due_ms = later;
    s.queue.extend(ready.into_iter().map(|(_, f)| f));
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
    descriptor_as(version, 0xf7)
}

fn descriptor_as(version: u16, packet: u8) -> Vec<u8> {
    let mut d = alloc::vec![0u8; 32];
    d[0] = 32;
    d[2] = packet; // 0xf7 the application, 0xff the bootloader
    d[4..6].copy_from_slice(&version.to_le_bytes());
    d[6..8].copy_from_slice(&236u16.to_le_bytes()); // report descriptor length
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
    if !s.up || (s.busy.0..s.busy.1).contains(&now_ms()) {
        return 0x1100_00c8;
    }
    match (reg, &data[1..]) {
        (0x0001, []) => match s.mode {
            Mode::NoDesc => {}
            Mode::BadDesc => s.queue.push_back(descriptor(0x0200)),
            Mode::LinuxTrace if !app_started(s) => s.queue.push_back(descriptor_as(0x0100, 0xff)),
            _ => s.queue.push_back(descriptor(0x0100)),
        },
        (0x0004, [0x0b, 0x00, 0x40, 0x00, 0x01, 0x3b, ..]) => {
            // The bootloader's answer, then the application's sentinel.
            let answer = now_ms() + if s.mode == Mode::SlowBoot { 150 } else { 10 };
            let app = match s.mode {
                Mode::LateApp => answer + 700,
                Mode::SlowBoot => answer + 80,
                Mode::EarlySentinel => now_ms() + 85,
                _ => now_ms() + 80,
            };
            match s.mode {
                Mode::EarlySentinel => s.due_ms.push((now_ms() + 15, alloc::vec![0, 0])),
                Mode::LinuxTrace => {}
                _ => s.due_ms.push((answer, alloc::vec![0x0b, 0, 0x30, 0, 0x01, 0, 0, 0, 0, 0, 0x17])),
            }
            for _ in 0..if s.mode == Mode::LinuxTrace { 5 } else { 1 } {
                s.due_ms.push((app, alloc::vec![0, 0]));
            }
            s.app_at = Some(app);
            if s.mode == Mode::LateApp {
                s.busy = (answer + 20, app);
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
                Mode::LinuxTrace if !app_started(s) => {}
                Mode::LinuxTrace => s.due_ms.push((now_ms() + 745, sysinfo())),
                Mode::SysinfoLate if s.sysinfo_asks == 1 => s.due_ms.push((now_ms() + 2500, sysinfo())),
                Mode::SysinfoSecond if s.sysinfo_asks == 1 => {}
                Mode::SysinfoSecond => s.due_ms.push((now_ms() + 770, sysinfo())),
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

static mut CLOCK: (u64, u64) = (0, 0);

fn ticks() -> u64 {
    #[cfg(target_arch = "x86_64")]
    {
        unsafe { core::arch::x86_64::_rdtsc() }
    }
    #[cfg(target_arch = "aarch64")]
    {
        let v: u64;
        unsafe { core::arch::asm!("mrs {}, cntvct_el0", out(reg) v, options(nomem, nostack)) };
        v
    }
}

/// Milliseconds since [`install`], from the TSC calibrated against `Stall` (x86-64) or
/// the generic timer (aarch64).
fn now_ms() -> u64 {
    let (t0, per) = unsafe { CLOCK };
    ticks().wrapping_sub(t0) / per.max(1)
}

fn init_clock() {
    let a = ticks();
    #[cfg(target_arch = "x86_64")]
    let per = {
        boot::stall(core::time::Duration::from_millis(20));
        (ticks() - a) / 20
    };
    #[cfg(target_arch = "aarch64")]
    let per = {
        let f: u64;
        unsafe { core::arch::asm!("mrs {}, cntfrq_el0", out(reg) f, options(nomem, nostack)) };
        f / 1000
    };
    unsafe { CLOCK = (a, per.max(1)) };
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
        reads_high: 0,
        app_at: None,
        rails: alloc::vec![((1, 2), (mode != Mode::RailOff, 3300)), ((0, 10), (true, 1800))],
        vreg_seq: Vec::new(),
        busy: (0, 0),
        nacks: 0,
        due_ms: Vec::new(),
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
    init_clock();
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
    let vreg: [u64; 8] = [
        0x10005,
        vreg_control as *const () as u64,
        vreg_set_level as *const () as u64,
        vreg_get_level as *const () as u64,
        vreg_other as *const () as u64,
        vreg_other as *const () as u64,
        vreg_get_status as *const () as u64,
        vreg_other as *const () as u64,
    ];
    unsafe {
        boot::install_protocol_interface(None, &VREG_GUID, leak(vreg) as *const c_void).expect("install vreg");
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
            "PARENT_FAKETOUCH calls configure={} group={} pin_read={} set={} other={} open={} read={} write={} open_now={} powered={} reads_high={}",
            c[0],
            c[1],
            c[2],
            c[3],
            c[4],
            c[5],
            c[6],
            c[7],
            s.open_handles,
            powered(s) as u8,
            s.reads_high
        ),
        format!("PARENT_FAKETOUCH seq {}", s.seq.join(" ")),
        format!("PARENT_FAKETOUCH vreg {} l3b={}", s.vreg_seq.join(" "), sensing(s) as u8),
    ]
}
