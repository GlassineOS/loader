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
    fn pins_ops(&self) -> Vec<&str> {
        self.ops.iter().filter(|o| o.starts_with("out")).map(|o| o.as_str()).collect()
    }
}

fn desc(version: u16) -> Vec<u8> {
    let mut d = vec![0u8; 32];
    d[0] = 32;
    d[4..6].copy_from_slice(&version.to_le_bytes());
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
        if !self.up {
            // No answer: the call runs to its timeout.
            self.now += t as u64;
            return Err("nack".into());
        }
        buf.fill(0);
        if let Some(f) = self.queue.front() {
            let n = f.len().min(buf.len());
            buf[..n].copy_from_slice(&f[..n]);
            if buf.len() >= f.len() || f.len() <= 2 {
                self.queue.pop_front();
            }
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
        if !self.up {
            self.now += t as u64;
            return Err("nack".into());
        }
        match (reg, data) {
            (0x0001, []) => match self.fail {
                Fail::NoDesc => {}
                Fail::BadDesc => self.queue.push_back(desc(0x0200)),
                _ => self.queue.push_back(desc(0x0100)),
            },
            (0x0004, d) if d == launch_app_cmd() => self.queue.push_back(vec![0x0b, 0, 0x30, 0, 0x01, 0, 0, 0, 0, 0, 0x17]),
            (0x0004, d) if d == SYSINFO_CMD_BYTES => {
                self.asks += 1;
                let empty = self.fail == Fail::NoSysinfo || (self.fail == Fail::SysinfoTwice && self.asks <= 2);
                self.queue.push_back(if empty { vec![0, 0] } else { sysinfo(1872, 1404, 0) });
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
    assert_eq!(f.ops.last().map(|s| s.as_str()), Some("out61=0"));
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
        (Fail::Dead, "empty"),
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
