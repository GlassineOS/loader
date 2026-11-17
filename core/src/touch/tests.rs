// SPDX-License-Identifier: GPL-2.0-or-later
//! The bring-up against a scripted controller.
use super::*;
use alloc::collections::VecDeque;
use alloc::string::ToString;
use alloc::vec;
use std::string::String as S;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Fail {
    None,
    PinRead,
    PinOut,
    Open,
    Write,
    NoDesc,
    BadDesc,
    SysinfoTwice,
    NoSysinfo,
    Dead,
    /// The bootloader answers launch-app after 150 ms.
    SlowBoot,
    /// The first three writes aren't taken.
    NackFirst,
    /// The application's sentinel 700 ms after the answer, the controller not answering
    /// its address until then.
    LateApp,
    /// No answer to launch-app (a controller already in its application).
    NoLaunch,
    /// The first GET_SYSINFO answered 2.5 s late.
    SysinfoLate,
    /// The first GET_SYSINFO never answered, the second after 770 ms.
    SysinfoSecond,
    /// Still in the bootloader at the descriptor: the first launch-app unanswered.
    Bootloader,
    /// A size-0 frame 15 ms after launch-app (too early to be the application's), the
    /// application's sentinel at 85 ms.
    EarlySentinel,
    /// Linux's own probe on the device: no answer to launch-app, the application's five
    /// sentinels 80 ms after it; until then the descriptor says bootloader (0xFF) and a
    /// GET_SYSINFO is lost; after them GET_SYSINFO answered 745 ms later.
    LinuxTrace,
    /// The interrupt never asserted.
    IrqStuck,
    /// pm660l LDO3 off (3.3 V set).
    RailOff,
    /// pm660l LDO3 off, set to 1.0 V (not the expected supply).
    RailOdd,
}

struct Fake {
    now: u64,
    out: [Option<bool>; 128],
    up: bool,
    reset_seen: bool,
    queue: VecDeque<Vec<u8>>,
    asks: u32,
    ops: Vec<S>,
    fail: Fail,
    open: bool,
    longest_call: u32,
    /// Frames due at a time.
    due: Vec<(u64, Vec<u8>)>,
    /// Neither reads nor writes are taken in this window.
    busy: (u64, u64),
    nacks: u32,
    launched: bool,
    /// Reads of the input register while the interrupt wasn't asserted.
    reads_high: u32,
    rdesc_reads: Vec<(u16, usize)>,
    /// When the application's sentinels come (after launch-app).
    app_at: Option<u64>,
    /// The regulators: (pmic, ldo) → (on, mV).
    rails: Vec<((u32, u32), (bool, u32))>,
}

impl Fake {
    fn new(fail: Fail, found_on: bool) -> Fake {
        let mut f = Fake {
            now: 1000,
            out: [None; 128],
            up: false,
            reset_seen: false,
            queue: VecDeque::new(),
            asks: 0,
            ops: Vec::new(),
            fail,
            open: false,
            longest_call: 0,
            due: Vec::new(),
            busy: (0, 0),
            nacks: 0,
            launched: false,
            reads_high: 0,
            rdesc_reads: Vec::new(),
            app_at: None,
            rails: vec![((0, 3), (true, 1000)), ((1, 3), (fail != Fail::RailOff && fail != Fail::RailOdd, if fail == Fail::RailOdd { 1000 } else { 3008 })), ((0, 11), (true, 1800))],
        };
        if found_on {
            f.out[61] = Some(true);
            f.out[62] = Some(true);
        }
        f
    }
    fn level(&self, p: u32) -> bool {
        self.out[p as usize].unwrap_or(p == PIN_IRQ && (self.queue.is_empty() || !self.up))
    }
    fn settle(&mut self) {
        if !(self.level(61) && self.level(62)) || self.fail == Fail::Dead {
            self.up = false;
            self.reset_seen = false;
            return;
        }
        if !self.level(66) {
            self.reset_seen = true;
            self.up = false;
            self.queue.clear();
        } else if self.reset_seen && !self.up {
            self.up = true;
            self.queue.push_back(vec![0, 0]);
        }
    }
    fn tick(&mut self) {
        let now = self.now;
        let (ready, later): (Vec<_>, Vec<_>) = core::mem::take(&mut self.due).into_iter().partition(|(t, _)| *t <= now);
        self.due = later;
        for (_, f) in ready {
            self.queue.push_back(f);
        }
    }
    fn pins_ops(&self) -> Vec<&str> {
        self.ops.iter().filter(|o| o.starts_with("out")).map(|o| o.as_str()).collect()
    }
}

fn desc(version: u16) -> Vec<u8> {
    desc_as(version, PACKET_APP)
}

fn desc_as(version: u16, packet: u8) -> Vec<u8> {
    let mut d = vec![0u8; 32];
    d[0] = 32;
    d[2] = packet;
    d[4..6].copy_from_slice(&version.to_le_bytes());
    d[6..8].copy_from_slice(&236u16.to_le_bytes());
    d[8..10].copy_from_slice(&2u16.to_le_bytes());
    d[12..14].copy_from_slice(&255u16.to_le_bytes());
    d[22..24].copy_from_slice(&0x04b4u16.to_le_bytes());
    d
}

fn sysinfo(res_x: u16, res_y: u16, max: u8) -> Vec<u8> {
    let mut f = vec![0u8; 51];
    f[0] = 51;
    f[2] = 0x1f;
    f[4] = 0x02;
    f[33 + 6..33 + 8].copy_from_slice(&res_x.to_le_bytes());
    f[33 + 8..33 + 10].copy_from_slice(&res_y.to_le_bytes());
    f[33 + 15] = max;
    f
}

impl Hw for Fake {
    fn now_ms(&mut self) -> u64 {
        self.now
    }
    fn stall_ms(&mut self, ms: u64) {
        self.now += ms;
    }
    fn pin_read(&mut self, p: u32) -> Result<bool, S> {
        self.ops.push(format!("read{p}"));
        if self.fail == Fail::PinRead {
            return Err("UNSUPPORTED".into());
        }
        Ok(self.level(p))
    }
    fn pin_out(&mut self, p: u32, h: bool) -> Result<(), S> {
        self.ops.push(format!("out{p}={}", h as u8));
        if self.fail == Fail::PinOut {
            return Err("UNSUPPORTED".into());
        }
        self.out[p as usize] = Some(h);
        self.settle();
        Ok(())
    }
    fn pin_in(&mut self, p: u32) -> Result<(), S> {
        self.ops.push(format!("in{p}"));
        self.out[p as usize] = None;
        Ok(())
    }
    fn irq_asserted(&mut self) -> Result<bool, S> {
        self.tick();
        Ok(self.fail != Fail::IrqStuck && self.up && !self.queue.is_empty())
    }
    fn read_reg(&mut self, reg: u16, buf: &mut [u8], _t: u32) -> Result<(), S> {
        assert!(self.open);
        self.rdesc_reads.push((reg, buf.len()));
        buf.fill(0);
        buf[..4].copy_from_slice(&[0x05, 0x0d, 0x09, 0x04]);
        Ok(())
    }
    fn rail_get(&mut self, pmic: u32, ldo: u32) -> Result<(bool, u32), S> {
        self.rails.iter().find(|r| r.0 == (pmic, ldo)).map(|r| r.1).ok_or_else(|| "INVALID_PARAMETER".into())
    }
    fn rail_enable(&mut self, pmic: u32, ldo: u32, on: bool) -> Result<(), S> {
        self.ops.push(format!("rail-en{pmic}.{ldo}={}", on as u8));
        let r = self.rails.iter_mut().find(|r| r.0 == (pmic, ldo)).ok_or("INVALID_PARAMETER")?;
        r.1.0 = on;
        Ok(())
    }
    fn open(&mut self) -> Result<(), S> {
        self.ops.push("open".into());
        if self.fail == Fail::Open {
            return Err("2".into());
        }
        self.open = true;
        Ok(())
    }
    fn read_input(&mut self, buf: &mut [u8], t: u32) -> Result<(), S> {
        assert!(self.open, "read on a closed bus");
        self.longest_call = self.longest_call.max(t);
        self.now += 1;
        if !self.up || (self.busy.0..self.busy.1).contains(&self.now) {
            // No answer to the address.
            self.now += 1;
            return Err("nack".into());
        }
        self.tick();
        if buf.len() == 2 && self.queue.is_empty() {
            self.reads_high += 1;
        }
        buf.fill(0);
        match self.queue.front() {
            Some(f) => {
                let n = f.len().min(buf.len());
                buf[..n].copy_from_slice(&f[..n]);
                if buf.len() >= f.len() || f.len() <= 2 {
                    self.queue.pop_front();
                }
            }
            // Nothing waiting: length 2.
            None => buf[0] = 2,
        }
        Ok(())
    }
    fn write(&mut self, reg: u16, data: &[u8], t: u32) -> Result<(), S> {
        assert!(self.open, "write on a closed bus");
        self.longest_call = self.longest_call.max(t);
        self.now += 1;
        self.ops.push(format!("wr{reg:04x}:{}", crate::hex(data)));
        if self.fail == Fail::Write {
            return Err("0x8002".into());
        }
        if self.fail == Fail::NackFirst && self.nacks < 3 {
            self.nacks += 1;
            return Err("0x110000c8".into());
        }
        if !self.up || (self.busy.0..self.busy.1).contains(&self.now) {
            return Err("0x110000c8".into());
        }
        match (reg, data) {
            (0x0001, []) => match self.fail {
                Fail::NoDesc => {}
                Fail::BadDesc => self.queue.push_back(desc(0x0200)),
                _ if self.fail == Fail::Bootloader && !self.launched => self.queue.push_back(desc_as(0x0100, PACKET_BL)),
                _ if self.fail == Fail::LinuxTrace && self.app_at.is_none_or(|t| self.now < t) => self.queue.push_back(desc_as(0x0100, PACKET_BL)),
                _ => self.queue.push_back(desc(0x0100)),
            },
            (0x0004, d) if d == launch_app_cmd() && self.fail == Fail::NoLaunch => {}
            (0x0004, d) if d == launch_app_cmd() && self.fail == Fail::Bootloader && !self.ops.iter().any(|o| o == "wr0001:") => {}
            (0x0004, d) if d == launch_app_cmd() => {
                // The answer, then the application's sentinel.
                let answer = self.now + if self.fail == Fail::SlowBoot { 150 } else { 10 };
                let app = match self.fail {
                    Fail::LateApp => answer + 700,
                    Fail::SlowBoot => answer + 80,
                    Fail::EarlySentinel => self.now + 85,
                    _ => self.now + 80,
                };
                match self.fail {
                    Fail::EarlySentinel => self.due.push((self.now + 15, vec![0, 0])),
                    Fail::LinuxTrace => {}
                    _ => self.due.push((answer, vec![0x0b, 0, 0x30, 0, 0x01, 0, 0, 0, 0, 0, 0x17])),
                }
                for _ in 0..if self.fail == Fail::LinuxTrace { 5 } else { 1 } {
                    self.due.push((app, vec![0, 0]));
                }
                self.app_at = Some(app);
                self.launched = true;
                if self.fail == Fail::LateApp {
                    self.busy = (answer + 20, app);
                }
            }
            (0x0004, d) if d == SYSINFO_CMD_BYTES => {
                self.asks += 1;
                let empty = self.fail == Fail::NoSysinfo || (self.fail == Fail::SysinfoTwice && self.asks <= 2);
                match self.fail {
                    Fail::LinuxTrace if self.app_at.is_none_or(|t| self.now < t) => {}
                    Fail::LinuxTrace => self.due.push((self.now + 745, sysinfo(1872, 1404, 0))),
                    Fail::SysinfoLate if self.asks == 1 => self.due.push((self.now + 2500, sysinfo(1872, 1404, 0))),
                    Fail::SysinfoSecond if self.asks == 1 => {}
                    Fail::SysinfoSecond => self.due.push((self.now + 770, sysinfo(1872, 1404, 0))),
                    _ => self.queue.push_back(if empty { vec![0, 0] } else { sysinfo(1872, 1404, 0) }),
                }
            }
            _ => {}
        }
        Ok(())
    }
    fn close(&mut self) {
        self.ops.push("close".into());
        self.open = false;
    }
}

fn line<'a>(up: &'a Up, k: &str) -> &'a str {
    up.lines.iter().find(|(kk, _)| *kk == k).map(|(_, v)| v.as_str()).unwrap_or("")
}

#[test]
fn the_power_up_order_and_the_power_down() {
    let mut f = Fake::new(Fail::None, false);
    let up = bring_up(&mut f, false);
    assert!(up.ok(), "{:?}", up.result);
    assert_eq!(&f.ops[..4], ["read61", "read62", "read66", "read67"]);
    assert_eq!(f.pins_ops(), ["out61=1", "out62=1", "out66=0", "out66=1", "out66=0", "out62=0", "out61=0"]);
    assert_eq!(up.sysinfo, Some(Sysinfo { res_x: 1872, res_y: 1404, max_tch: 10 }));
    assert!(line(&up, "touch.hid").starts_with("vendor=0x04b4"), "{:?}", up.lines);
    assert_eq!(line(&up, "touch.res"), "1872x1404 max_tch=10 ask=1");
    assert!(line(&up, "touch.result").starts_with("ok"));
    assert_eq!(f.ops.last().map(|s| s.as_str()), Some("in61"));
    assert!(f.ops.contains(&"close".to_string()) && !f.open);
    // The commands as mainline puts them on the wire.
    assert!(f.ops.contains(&format!("wr0004:{}", crate::hex(&launch_app_cmd()))));
    assert!(f.ops.contains(&"wr0001:".to_string()));
    assert!(f.ops.contains(&"wr0004:05002f0002".to_string()));
    // Every wait inside the deadline, every call at most CALL_MS.
    assert!(f.now - 1000 < DEADLINE_MS && f.longest_call as u64 <= CALL_MS);
}

#[test]
fn kept_for_the_tap_source() {
    let mut f = Fake::new(Fail::None, false);
    let mut up = bring_up(&mut f, true);
    assert!(up.ok() && f.open && f.level(61) && f.level(62) && f.level(66));
    power_down(&mut f, &mut up);
    assert!(!f.open && !f.level(61) && !f.level(62) && !f.level(66));
}

#[test]
fn found_powered_is_left_powered() {
    let mut f = Fake::new(Fail::None, true);
    let up = bring_up(&mut f, false);
    assert!(up.ok(), "{:?}", up.result);
    assert_eq!(f.pins_ops(), ["out66=0", "out66=1", "out66=1"]);
    assert!(f.level(61) && f.level(62));
    assert_eq!(line(&up, "touch.power"), "found on: left as found");
}

#[test]
fn a_failure_at_each_step_powers_down() {
    for (fail, step) in [
        (Fail::PinRead, "found"),
        (Fail::PinOut, "power"),
        (Fail::Open, "open"),
        (Fail::Write, "hid"),
        (Fail::NoDesc, "hid"),
        (Fail::BadDesc, "hid"),
        (Fail::NoSysinfo, "sysinfo"),
        (Fail::Dead, "hid"),
    ] {
        let mut f = Fake::new(fail, false);
        let up = bring_up(&mut f, true);
        let e = up.result.as_ref().expect_err(&format!("{fail:?}"));
        assert!(e.starts_with(step), "{fail:?}: {e}");
        assert!(!f.open, "{fail:?}: bus left open");
        if fail != Fail::PinRead && fail != Fail::PinOut {
            assert!(!f.level(61) && !f.level(62) && !f.level(66), "{fail:?}: left powered");
        }
        if fail == Fail::PinRead {
            assert!(f.pins_ops().is_empty(), "nothing driven when the pins can't be read");
        }
        assert!(f.now - 1000 <= DEADLINE_MS + CALL_MS + 10, "{fail:?}: {} ms", f.now - 1000);
        assert!(line(&up, "touch.result").starts_with("failed"));
    }
}

#[test]
fn sysinfo_asked_again() {
    let mut f = Fake::new(Fail::SysinfoTwice, false);
    let up = bring_up(&mut f, false);
    assert!(up.ok(), "{:?}", up.result);
    assert_eq!(f.asks, 3);
    assert!(line(&up, "touch.res").ends_with("ask=3"));
}

#[test]
fn the_launch_command_carries_its_crc() {
    let c = launch_app_cmd();
    assert_eq!(crc_itu_t(0xffff, b"123456789"), 0x29b1);
    assert_eq!(u16::from_le_bytes([c[8], c[9]]), crc_itu_t(0xffff, &[0x01, 0x3b, 0, 0]));
    assert_eq!((c[0], c[2], c[10]), (11, 0x40, 0x17));
}

#[test]
fn touch_reports() {
    let rec = |id: u8, ev: u8, x: u16, y: u16| {
        let mut r = vec![0u8; 10];
        r[1] = id | (ev << 5);
        r[2..4].copy_from_slice(&x.to_le_bytes());
        r[4..6].copy_from_slice(&y.to_le_bytes());
        r
    };
    let frame = |recs: &[Vec<u8>]| {
        let mut f = vec![0u8; 7];
        f[2] = 1;
        f[5] = recs.len() as u8;
        for r in recs {
            f.extend_from_slice(r);
        }
        let n = f.len() as u16;
        f[..2].copy_from_slice(&n.to_le_bytes());
        f
    };
    assert_eq!(parse_touch(&frame(&[rec(0, 1, 100, 200)])), Some(Some((100, 200))));
    assert_eq!(parse_touch(&frame(&[rec(0, 3, 1, 2), rec(1, 2, 300, 400)])), Some(Some((300, 400))));
    assert_eq!(parse_touch(&frame(&[rec(0, 3, 1, 2)])), Some(None));
    assert_eq!(parse_touch(&frame(&[])), Some(None));
    let mut other = frame(&[]);
    other[2] = 3;
    assert_eq!(parse_touch(&other), None);
    // A record count past the frame's end stops at the end.
    let mut short = frame(&[rec(0, 3, 1, 2)]);
    short[5] = 5;
    assert_eq!(parse_touch(&short), Some(None));
}

#[test]
fn frames_and_descriptors() {
    assert!(hid_ok(&desc(0x0100)) && !hid_ok(&desc(0x0200)) && !hid_ok(&desc(0x0100)[..30]));
    assert_eq!(parse_sysinfo(&sysinfo(1872, 1404, 7)), Some(Sysinfo { res_x: 1872, res_y: 1404, max_tch: 7 }));
    assert_eq!(parse_sysinfo(&sysinfo(1, 1, 1)[..48]), None);
}

#[test]
fn the_mapping_to_portrait() {
    // The lab's calibration: portrait u = 1403 - raw y, v = 1871 - raw x.
    let r = crate::menu::Range { min_x: 0, max_x: RES.0 as u64 - 1, min_y: 0, max_y: RES.1 as u64 - 1 };
    for (x, y) in [(0u64, 0u64), (1871, 1403), (100, 1300), (1223, 701)] {
        let (u, v) = crate::menu::to_portrait(x, y, r, ORIENT, 1872, 1404).unwrap();
        assert_eq!((u as u64, v as u64), (1403 - y, 1871 - x), "{x},{y}");
    }
}

#[test]
fn slow_boot_late_app_and_nacks_are_waited_for() {
    for fail in [Fail::SlowBoot, Fail::LateApp, Fail::NackFirst] {
        let mut f = Fake::new(fail, false);
        let up = bring_up(&mut f, false);
        assert!(up.ok(), "{fail:?}: {:?} {:?}", up.result, up.lines);
        assert!(line(&up, "touch.launch").starts_with("app sentinel "), "{fail:?}: {:?}", up.lines);
        assert!(line(&up, "touch.wait.reset").contains(" ok"), "{fail:?}: {:?}", up.lines);
        assert!(line(&up, "touch.wait.app").contains(" ok"), "{fail:?}: {:?}", up.lines);
        let ms = |k: &str| line(&up, k).split(' ').next().unwrap().parse::<u64>().unwrap();
        match fail {
            Fail::SlowBoot => assert!((225..=260).contains(&ms("touch.wait.app")), "{:?}", up.lines),
            Fail::LateApp => assert!((700..=760).contains(&ms("touch.wait.app")), "{:?}", up.lines),
            _ => assert!(f.ops.iter().filter(|o| o.starts_with("wr0004:0b00")).count() == 4, "{:?}", f.ops),
        }
        assert!(f.now - 1000 < DEADLINE_MS);
    }
}

#[test]
fn a_launch_without_an_answer_isnt_fatal() {
    let mut f = Fake::new(Fail::NoLaunch, false);
    let up = bring_up(&mut f, false);
    assert!(up.ok(), "{:?}", up.lines);
    assert_eq!(line(&up, "touch.launch"), "no sentinel");
    let ms: u64 = line(&up, "touch.wait.app").split(' ').next().unwrap().parse().unwrap();
    assert!(line(&up, "touch.wait.app").contains("timeout") && (APP_WAIT_MS - 10..=APP_WAIT_MS).contains(&ms), "{:?}", up.lines);
}

#[test]
fn sysinfo_is_waited_for_like_linux() {
    let mut f = Fake::new(Fail::SysinfoLate, false);
    let up = bring_up(&mut f, false);
    assert!(up.ok(), "{:?}", up.lines);
    assert_eq!(f.asks, 1, "sent once");
    assert!(line(&up, "touch.res").ends_with("ask=1"));
    let ms: u64 = line(&up, "touch.wait.sysinfo").split(' ').next().unwrap().parse().unwrap();
    assert!((2450..=2600).contains(&ms), "{:?}", up.lines);
    let mut f = Fake::new(Fail::SysinfoSecond, false);
    let up = bring_up(&mut f, false);
    assert!(up.ok(), "{:?}", up.lines);
    assert_eq!(f.asks, 2);
    assert!(line(&up, "touch.res").ends_with("ask=2"));
    assert!(f.now - 1000 < DEADLINE_MS);
    // Never answered: three sends, then the failure, inside the deadline.
    let mut f = Fake::new(Fail::NoSysinfo, false);
    let up = bring_up(&mut f, false);
    assert_eq!(f.asks, SYSINFO_SENDS);
    assert!(up.result.as_ref().unwrap_err().starts_with("sysinfo") && f.now - 1000 <= DEADLINE_MS + CALL_MS + 10);
}

#[test]
fn the_mode_from_the_descriptor() {
    let mut f = Fake::new(Fail::None, false);
    let up = bring_up(&mut f, false);
    assert_eq!(line(&up, "touch.mode"), "app");
    let mut f = Fake::new(Fail::Bootloader, false);
    let up = bring_up(&mut f, false);
    assert!(up.ok(), "{:?}", up.lines);
    assert_eq!((line(&up, "touch.mode"), line(&up, "touch.mode.bl")), ("bl", "app"), "{:?}", up.lines);
    assert_eq!(line(&up, "touch.launch"), "no sentinel");
    assert!(line(&up, "touch.launch.bl").starts_with("app sentinel"), "{:?}", up.lines);
    assert_eq!(f.ops.iter().filter(|o| o.starts_with("wr0004:0b00")).count(), 2);
}

#[test]
fn reads_only_with_the_interrupt_asserted() {
    let mut f = Fake::new(Fail::None, false);
    let up = bring_up(&mut f, false);
    assert!(up.ok(), "{:?}", up.lines);
    assert_eq!(f.reads_high, 0, "a read without the interrupt: {:?}", up.lines);
    assert!(line(&up, "touch.wait.hid").contains("irq_low=1 reads=1"), "{:?}", up.lines);
    // The report descriptor isn't read (off, as Linux).
    assert!(f.rdesc_reads.is_empty() && line(&up, "touch.rdesc").is_empty());
}

#[test]
fn the_report_descriptor_from_the_hid_descriptor() {
    let mut f = Fake::new(Fail::None, false);
    f.open = true;
    let mut up = Up::new(false);
    up.opened = true;
    let d = desc(0x0100);
    let le = |o: usize| u16::from_le_bytes([d[o], d[o + 1]]);
    report_desc(&mut f, &mut up, 1_000_000, le(8), le(6), le(12));
    assert_eq!(f.rdesc_reads, [(2, 236)]);
    assert_eq!(line(&up, "touch.rdesc"), "reg=0x0002 len=236 050d0904000000000000000000000000");
}

#[test]
fn an_interrupt_that_never_moves_falls_back_to_50_ms_polls() {
    let mut f = Fake::new(Fail::IrqStuck, false);
    let up = bring_up(&mut f, false);
    assert!(up.ok(), "{:?}", up.lines);
    for k in ["touch.wait.reset", "touch.wait.app", "touch.wait.hid", "touch.wait.sysinfo"] {
        let v = line(&up, k);
        let ms: u64 = v.split(' ').next().unwrap().parse().unwrap();
        let most = if k == "touch.wait.app" { 2 * FALLBACK_MS + 20 } else { FALLBACK_MS + 20 };
        assert!(v.contains(" ok irq_low=0") && (FALLBACK_MS..most).contains(&ms), "{k}: {v}");
    }
}

#[test]
fn the_fallback_is_for_a_bring_up_without_the_interrupt_not_per_wait() {
    // The interrupt seen once (the reset sentinel): later waits read only with it.
    let mut f = Fake::new(Fail::SysinfoLate, false);
    let up = bring_up(&mut f, false);
    assert!(up.ok() && up.irq_seen, "{:?}", up.lines);
    assert!(line(&up, "touch.wait.sysinfo").contains("irq_low=1 reads=1"), "{:?}", up.lines);
    assert_eq!(f.reads_high, 0);
}

#[test]
fn a_sentinel_within_30_ms_of_launch_is_not_the_application() {
    let mut f = Fake::new(Fail::EarlySentinel, false);
    let up = bring_up(&mut f, false);
    assert!(up.ok(), "{:?}", up.lines);
    let l = line(&up, "touch.launch");
    let at: u64 = l.strip_prefix("app sentinel ").unwrap().split(' ').next().unwrap().parse().unwrap();
    assert!((85..=95).contains(&at) && l.ends_with(" ms drained=0 dropped=1"), "{l}");
}

#[test]
fn the_linux_probe_order() {
    let mut f = Fake::new(Fail::LinuxTrace, false);
    let up = bring_up(&mut f, false);
    assert!(up.ok(), "{:?}", up.lines);
    // The application's five sentinels: one taken, four drained; the descriptor then the
    // application's; GET_SYSINFO once, after them, answered on the interrupt.
    assert!(line(&up, "touch.launch").ends_with(" ms drained=4"), "{:?}", up.lines);
    assert_eq!(line(&up, "touch.mode"), "app");
    assert_eq!(f.asks, 1);
    let v = line(&up, "touch.wait.sysinfo");
    let ms: u64 = v.split(' ').next().unwrap().parse().unwrap();
    assert!((745..=760).contains(&ms) && v.contains(" ok irq_low=1 reads=1"), "{v}");
    assert_eq!(f.reads_high, 0);
    let launch = f.ops.iter().position(|o| o.starts_with("wr0004:0b00")).unwrap();
    let hid = f.ops.iter().position(|o| o == "wr0001:").unwrap();
    assert!(launch < hid);
}

#[test]
fn the_descriptor_asked_again_when_the_interrupt_never_comes() {
    let mut f = Fake::new(Fail::NoDesc, false);
    let up = bring_up(&mut f, false);
    assert_eq!(f.ops.iter().filter(|o| *o == "wr0001:").count(), 2, "{:?}", up.lines);
    assert_eq!(line(&up, "touch.hid.resent"), "true");
    assert_eq!(up.result, Err("hid: no answer".to_string()));
}

#[test]
fn the_supplies_are_read_and_the_pins_released_as_inputs() {
    let mut f = Fake::new(Fail::None, false);
    let up = bring_up(&mut f, false);
    assert!(up.ok(), "{:?}", up.lines);
    assert_eq!(line(&up, "touch.rail.l3b"), "on 3008 mV");
    assert_eq!(line(&up, "touch.rail.l11a"), "on 1800 mV");
    assert!(line(&up, "touch.rail.on").is_empty() && !f.ops.iter().any(|o| o.starts_with("rail-")));
    // After the switches low: the three driven pins back to inputs.
    let n = f.ops.len();
    assert_eq!(&f.ops[n - 6..], ["out66=0", "out62=0", "out61=0", "in66", "in62", "in61"]);
    assert_eq!(line(&up, "touch.release"), "inputs: 66, 62, 61");
}

#[test]
fn touch_rail_turns_on_what_is_off_and_off_again() {
    let mut f = Fake::new(Fail::RailOff, false);
    let up = bring_up_with(&mut f, false, true);
    assert!(up.ok(), "{:?}", up.lines);
    assert_eq!(line(&up, "touch.rail.l3b"), "off 3008 mV");
    assert_eq!(line(&up, "touch.rail.on"), "l3b turned on on 3008 mV, l11a already on 1800 mV");
    let at = |o: &str| f.ops.iter().position(|x| x == o).unwrap_or_else(|| panic!("{o}: {:?}", f.ops));
    // Only the enable: no level is ever set.
    assert_eq!(f.ops.iter().filter(|o| o.starts_with("rail-")).collect::<Vec<_>>(), ["rail-en1.3=1", "rail-en1.3=0"]);
    assert!(at("rail-en1.3=1") < at("out61=1"));
    assert!(at("out61=0") < at("rail-en1.3=0") && at("rail-en1.3=0") < at("in66"));
    assert_eq!(line(&up, "touch.rail.off"), "l3b off");
    assert!(!f.rails.iter().find(|r| r.0 == (1, 3)).unwrap().1.0);
    // Not the expected supply: left alone.
    let mut f = Fake::new(Fail::RailOdd, false);
    let up = bring_up_with(&mut f, false, true);
    assert_eq!(line(&up, "touch.rail.on"), "l3b refused 1000 mV, l11a already on 1800 mV");
    assert!(!f.ops.iter().any(|o| o.starts_with("rail-")) && line(&up, "touch.rail.off").is_empty());
    // Found powered: no supply touched.
    let mut f = Fake::new(Fail::RailOff, true);
    let up = bring_up_with(&mut f, false, true);
    assert_eq!(line(&up, "touch.rail.on"), "skipped found on");
    assert!(!f.ops.iter().any(|o| o.starts_with("rail-")));
    // Without touch_rail: nothing written even with the supply off.
    let mut f = Fake::new(Fail::RailOff, false);
    bring_up(&mut f, false);
    assert!(!f.ops.iter().any(|o| o.starts_with("rail-")));
}
