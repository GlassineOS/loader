// SPDX-License-Identifier: GPL-2.0-or-later
//! The start menu on a firmware-refreshed panel (loader spec 004; boot
//! `docs/handback.md`, "The start menu"): the PC list's entries as rows the person taps,
//! drawn by LVGL in portrait into the `epd-fw` framebuffer and refreshed as the changed
//! rectangles. What it decides and writes is in `glassine_loader_core::menu`; this is the
//! firmware side: the tap source, the loop and its watchdog. Taps are the only input
//! (004 FR-010): keys are logged and do nothing.
use alloc::boxed::Box;
use alloc::vec::Vec;
use core::time::Duration;
use glassine_loader_core::deploys::{self, Action, Entry};
use glassine_loader_core::look;
use glassine_loader_core::menu::{self, Debounce, Orient, Range, Why};
use glassine_loader_core::plan::Choice;
use glassine_loader_core::record::InUse;
use uefi::boot::{self, SearchType};
use uefi::proto::console::text::Key;
use uefi::{Handle, Status};
use uefi_raw::protocol::console::{AbsolutePointerProtocol, AbsolutePointerState, KeyData, SimpleTextInputExProtocol};

use crate::display::epdfw::EpdFw;
use crate::{log, ui};

/// How the touch panel's axes lie on the display panel's native axes (spec 004 FR-011).
/// **Unverified on hardware**: the one place to change after the first touch test. With
/// all three false, the pointer's X runs along the panel's native x (its 1872 pixels)
/// and its Y along the native y.
pub const TOUCH: Orient = Orient { swap: false, flip_x: false, flip_y: false };

/// How long the picture must stay unchanged before the changed rows are refreshed.
const SETTLE_MS: u64 = 100;

unsafe extern "C" {
    fn shim_epd_pointer(read: extern "C" fn(*mut i32, *mut i32, *mut i32));
    fn shim_epd_menu(
        title: *const core::ffi::c_char,
        reason: *const core::ffi::c_char,
        names: *const *const core::ffi::c_char,
        lines: *const *const core::ffi::c_char,
        icons: *const i32,
        n: i32,
        dflt: i32,
        foot: *const core::ffi::c_char,
    );
    fn shim_epd_pick(i: i32);
    fn shim_epd_notouch(default_line: *const core::ffi::c_char, foot: *const core::ffi::c_char);
    fn shim_list_choice() -> i32;
    fn shim_hide_list();
    fn shim_handle() -> u32;
}

/// Where taps come from (spec 004 FR-010): a source gives the touch's state, pressed or
/// not, at a position on the portrait panel (`H`×`W`, (0, 0) top left). The menu sees
/// only this; a touch driver of the loader's own for the panel's controller is another
/// source behind the same interface.
pub trait TapSource {
    /// What it is, for the log.
    fn name(&self) -> &'static str;
    /// One read, never waiting: `None` when nothing changed since the last read.
    fn poll(&mut self) -> Option<(bool, u32, u32)>;
    /// Whether it gives no more taps (a bring-up or a read that failed): the menu then
    /// can only wait for the default.
    fn failed(&self) -> bool {
        false
    }
}

/// A firmware absolute pointer of a device's own, read raw: `GetState` only, never
/// `Reset`, never a wait on its event.
struct FirmwarePointer {
    iface: u64,
    range: Range,
    /// The native panel.
    w: u32,
    h: u32,
    failed: bool,
    down: bool,
}

/// The tap source for a native `w`×`h` panel, if the firmware has one: an absolute
/// pointer of a device's own whose Mode (where the memory map says RAM) has usable
/// ranges. The console splitter's on the console-in handle is not one: with no touch
/// driver bound under it, it never reports (spec 004 FR-010). With `touch=on` the
/// loader's own touch driver comes first (spec 005 FR-011).
pub fn tap_source(w: u32, h: u32) -> Option<Box<dyn TapSource>> {
    if let Some(s) = crate::touch::source(w, h) {
        return Some(s);
    }
    let handles: Vec<Handle> = boot::locate_handle_buffer(SearchType::ByProtocol(&AbsolutePointerProtocol::GUID))
        .map(|b| b.iter().copied().collect())
        .unwrap_or_default();
    let conin = uefi::table::system_table_raw().and_then(|t| unsafe { Handle::from_ptr(t.as_ref().stdin_handle) });
    let ram = crate::fwgop::Ram::read();
    for hd in handles {
        if Some(hd) == conin {
            log!("TOUCH_SKIPPED the console splitter's pointer");
            continue;
        }
        let Some(iface) = crate::fwgop::interface_of(hd, &AbsolutePointerProtocol::GUID) else { continue };
        if !ram.holds(iface, 32) {
            continue;
        }
        let mode = unsafe { crate::fwgop::rd64(iface + 24) };
        if mode == 0 || mode % 8 != 0 || !ram.holds(mode, 52) {
            log!("TOUCH_SKIPPED Mode 0x{mode:x} unreadable");
            continue;
        }
        let r = |o: u64| unsafe { crate::fwgop::rd64(mode + o) };
        let range = Range { min_x: r(0), max_x: r(24), min_y: r(8), max_y: r(32) };
        log!(
            "TOUCH_DEVICE firmware-pointer x={}..{} y={}..{} orient=swap:{} flip_x:{} flip_y:{}",
            range.min_x,
            range.max_x,
            range.min_y,
            range.max_y,
            TOUCH.swap as u8,
            TOUCH.flip_x as u8,
            TOUCH.flip_y as u8
        );
        if !range.usable() {
            log!("TOUCH_SKIPPED unusable ranges");
            continue;
        }
        return Some(Box::new(FirmwarePointer { iface, range, w, h, failed: false, down: false }));
    }
    None
}

impl TapSource for FirmwarePointer {
    fn name(&self) -> &'static str {
        "firmware-pointer"
    }

    fn failed(&self) -> bool {
        self.failed
    }

    fn poll(&mut self) -> Option<(bool, u32, u32)> {
        if self.failed {
            return None;
        }
        type GetState = unsafe extern "efiapi" fn(u64, *mut AbsolutePointerState) -> Status;
        let f: GetState = unsafe { core::mem::transmute(crate::fwgop::rd64(self.iface + 8) as usize) };
        let mut s = AbsolutePointerState::default();
        let st = unsafe { f(self.iface, &mut s) };
        if st == Status::NOT_READY {
            return None;
        }
        if st != Status::SUCCESS {
            log!("TOUCH GetState {:?}: no more taps", st);
            self.failed = true;
            return None;
        }
        let pressed = s.active_buttons & 1 != 0;
        let (u, v) = menu::to_portrait(s.current_x, s.current_y, self.range, TOUCH, self.w, self.h).unwrap_or((0, 0));
        if pressed && !self.down {
            log!("TOUCH raw={},{} portrait={},{}", s.current_x, s.current_y, u, v);
        }
        self.down = pressed;
        Some((pressed, u, v))
    }
}

/// The menu's taps: the source and its debounce.
struct Taps {
    src: Box<dyn TapSource>,
    deb: Debounce,
    down: bool,
}

static mut TAPS: Option<Taps> = None;
/// The last input (a press), for the timeout.
static mut LAST_INPUT: u64 = 0;

extern "C" fn pointer_read(x: *mut i32, y: *mut i32, pressed: *mut i32) {
    let now = crate::arch::now_ms();
    // SAFETY: LVGL calls this only from shim calls of `run`, one at a time.
    let t = unsafe { (*core::ptr::addr_of_mut!(TAPS)).as_mut() };
    let (down, u, v) = match t {
        Some(t) => {
            let s = t.src.poll();
            let out = t.deb.feed(now, s);
            if out.0 && !t.down {
                unsafe { LAST_INPUT = now };
            }
            t.down = out.0;
            out
        }
        None => (false, 0, 0),
    };
    unsafe {
        *x = u as i32;
        *y = v as i32;
        *pressed = down as i32;
    }
}

/// Whether the tap source gives no more taps.
fn taps_failed() -> bool {
    // SAFETY: read on the loop's thread, the only one.
    unsafe { (*core::ptr::addr_of!(TAPS)).as_ref().is_some_and(|t| t.src.failed()) }
}

/// Whether a touch is down (after the debounce).
fn touch_down() -> bool {
    // SAFETY: read on the loop's thread, the only one.
    unsafe { (*core::ptr::addr_of!(TAPS)).as_ref().is_some_and(|t| t.down) }
}

/// The console's keys, logged only (spec 004 FR-010): the extended protocol where the
/// console-in handle has one (the shift and toggle state logged too), else the plain
/// one.
struct Keys {
    ex: Option<u64>,
}

impl Keys {
    fn find() -> Keys {
        let conin = uefi::table::system_table_raw().and_then(|t| unsafe { Handle::from_ptr(t.as_ref().stdin_handle) });
        let ram = crate::fwgop::Ram::read();
        let ex = conin.and_then(|h| crate::fwgop::interface_of(h, &SimpleTextInputExProtocol::GUID)).filter(|i| ram.holds(*i, 48));
        log!("KEYS via={}", if ex.is_some() { "ex" } else { "plain" });
        Keys { ex }
    }

    /// One key, if one is waiting: (scan code, character). Every key is logged (`KEY`).
    fn read(&mut self) -> Option<(u16, u16)> {
        if let Some(i) = self.ex {
            type ReadEx = unsafe extern "efiapi" fn(u64, *mut KeyData) -> Status;
            let f: ReadEx = unsafe { core::mem::transmute(crate::fwgop::rd64(i + 8) as usize) };
            let mut k: KeyData = unsafe { core::mem::zeroed() };
            let st = unsafe { f(i, &mut k) };
            if st == Status::SUCCESS {
                let (sc, ch) = (k.key.scan_code, u16::from(k.key.unicode_char));
                log!(
                    "KEY scan=0x{:04x} char=0x{:04x} shift=0x{:08x} toggle=0x{:02x}",
                    sc,
                    ch,
                    k.key_state.key_shift_state.bits(),
                    k.key_state.key_toggle_state.bits()
                );
                return Some((sc, ch));
            }
            if st != Status::NOT_READY {
                log!("KEYS ReadKeyStrokeEx {:?}: the plain input", st);
                self.ex = None;
            }
            return None;
        }
        let (sc, ch) = match ui::read_key()? {
            Key::Special(s) => (s.0, 0),
            Key::Printable(c) => (0, u16::from(c)),
        };
        log!("KEY scan=0x{:04x} char=0x{:04x}", sc, ch);
        Some((sc, ch))
    }
}


/// How a list ended.
enum Shown {
    Pick(usize),
    Timeout,
}

fn c(s: &str) -> alloc::ffi::CString {
    alloc::ffi::CString::new(s.replace('\0', " ")).unwrap()
}

/// Shows one list and runs it until a pick or the timeout, refreshing what
/// changed after each round of LVGL and re-arming the watchdog. `log_title` and the
/// entries' labels go to the log; the cards are drawn as `look` gives them (six at most,
/// Back kept). `dflt`: the entry the timeout takes, drawn as the default.
fn show(epd: &mut EpdFw, keys: &mut Keys, log_title: &str, title: &str, reason: &str, items: &[Entry], dflt: Option<usize>) -> Shown {
    let shown = look::fit(items.len());
    let cards: Vec<look::Card> = shown.iter().map(|&i| look::card(&items[i])).collect();
    let names: Vec<alloc::ffi::CString> = cards.iter().map(|c| self::c(&c.name)).collect();
    let lines: Vec<alloc::ffi::CString> = cards.iter().map(|c| self::c(&c.line)).collect();
    let np: Vec<*const core::ffi::c_char> = names.iter().map(|s| s.as_ptr()).collect();
    let lp: Vec<*const core::ffi::c_char> = lines.iter().map(|s| s.as_ptr()).collect();
    let icons: Vec<i32> = cards.iter().map(|c| c.icon as i32).collect();
    let d = dflt.and_then(|i| shown.iter().position(|&s| s == i));
    let foot = c(if shown.len() < items.len() { look::FOOT_CUT } else { look::FOOT });
    let (ct, cr) = (c(title), c(reason));
    unsafe {
        shim_epd_menu(ct.as_ptr(), cr.as_ptr(), np.as_ptr(), lp.as_ptr(), icons.as_ptr(), np.len() as i32, d.map_or(-1, |i| i as i32), foot.as_ptr())
    };
    log!("MENU_SHOWN {} ({} entries)", log_title, items.len());
    for (i, it) in items.iter().enumerate() {
        log!("MENU_ENTRY {} {}", i, it.label);
    }
    if shown.len() < items.len() {
        log!("MENU_CUT {} of {} entries shown", shown.len(), items.len());
    }
    let mut rearmed = crate::arch::now_ms();
    let mut drawn = 0u64;
    let mut no_touch = false;
    let r = loop {
        // Keys only logged (spec 004 FR-010): a key is no input for the timeout either.
        let _ = keys.read();
        // One refresh per change: only once LVGL has drawn nothing more for SETTLE_MS
        // and no touch is down, so the rows a tap or a key changes over several of
        // LVGL's rounds (its input is read every 30 ms) go out together.
        let before = epd.dirty_len();
        unsafe { shim_handle() };
        // The tap source gave up (its bring-up failed): the cards that can't be picked
        // any more are drawn as such, once.
        if !no_touch && taps_failed() {
            no_touch = true;
            log!("MENU_NO_TOUCH the menu waits for the default");
            let (dl, f) = (c(look::LINE_NO_TOUCH), c(look::FOOT_NO_TOUCH));
            unsafe {
                shim_epd_notouch(dl.as_ptr(), f.as_ptr());
                shim_handle();
            }
        }
        if epd.dirty_len() != before {
            drawn = crate::arch::now_ms();
        }
        if !touch_down() && crate::arch::now_ms().saturating_sub(drawn) >= SETTLE_MS && epd.refresh_dirty() > 0 {
            rearmed = crate::arch::now_ms();
        }
        let i = unsafe { shim_list_choice() };
        if i >= 0 {
            break Shown::Pick(shown[i as usize]);
        }
        let now = crate::arch::now_ms();
        if now.saturating_sub(unsafe { LAST_INPUT }) >= menu::TIMEOUT_MS {
            // The default's card turns black as a tap would turn it.
            if let Some(i) = d {
                unsafe { shim_epd_pick(i as i32) };
            }
            break Shown::Timeout;
        }
        if now.saturating_sub(rearmed) >= menu::REARM_MS / 2 {
            crate::watchdog::arm();
            rearmed = now;
        }
        boot::stall(Duration::from_millis(5));
    };
    // The tapped card's last look reaches the panel before the splash.
    unsafe { shim_handle() };
    epd.refresh_dirty();
    r
}

/// Runs the menu on `epd` over the record read at entry (`rec`) and returns the pick.
/// The caller has made the menu write; the splash and the pick's write follow.
pub fn run(epd: &mut EpdFw, rec: &InUse, src: Box<dyn TapSource>, other: Option<&str>, why: Why) -> Choice {
    let (w, h) = epd.size_native();
    // What the framebuffer holds before the menu draws (the firmware's or the last start's
    // picture), to compare with the CRCs before each refresh (`display_crc=1` only).
    if epd.crc_log {
        log!("EPD_FB 0 crc={:08x} at the menu's start", epd.crc());
    }
    unsafe {
        log!("MENU_INPUT {}", src.name());
        TAPS = Some(Taps { src, deb: Debounce::default(), down: false });
        LAST_INPUT = crate::arch::now_ms();
    }
    let mut keys = Keys::find();
    // LVGL over a white portrait picture, the size of the panel.
    let (lw, lh) = (h, w);
    let white = alloc::vec![0x00FF_FFFFu32; (lw as usize) * (lh as usize)];
    // SAFETY: the display outlives the UI: `epd` stays borrowed until this returns, and
    // nothing of the UI is called after it.
    let d: &mut (dyn crate::display::Display + 'static) = unsafe { &mut *(epd as *mut EpdFw as *mut dyn crate::display::Display) };
    ui::start(d, white);
    unsafe { shim_epd_pointer(pointer_read) };

    let main = deploys::main_list(Some(&rec.rec), other);
    let pick = loop {
        let i = match show(epd, &mut keys, "Start GlassineOS", look::TITLE, look::reason(why), &main, main.iter().position(|e| e.action == Action::Continue)) {
            Shown::Pick(i) => i,
            Shown::Timeout => {
                log!("MENU_TIMEOUT {} s: GlassineOS", menu::TIMEOUT_MS / 1000);
                break Choice::Ordinary;
            }
        };
        log!("MENU_PICK {} {}", i, main[i].label);
        match &main[i].action {
            Action::Continue => break Choice::Ordinary,
            Action::Deploy(id) => break Choice::Deploy(id.clone()),
            Action::PreviousKernel => break Choice::PreviousKernel,
            Action::Recovery => break Choice::Recovery,
            Action::Other(n) => break Choice::Other(n.clone()),
            Action::Back => {}
            Action::More => {
                let more = deploys::more_list(&rec.rec);
                match show(epd, &mut keys, "Deployments", look::TITLE_MORE, look::REASON_MORE, &more, None) {
                    Shown::Pick(j) => {
                        log!("MENU_PICK more {} {}", j, more[j].label);
                        if let Action::Deploy(id) = &more[j].action {
                            break Choice::Deploy(id.clone());
                        }
                    }
                    Shown::Timeout => {
                        log!("MENU_TIMEOUT {} s: GlassineOS", menu::TIMEOUT_MS / 1000);
                        break Choice::Ordinary;
                    }
                }
            }
        }
    };
    unsafe {
        shim_hide_list();
        TAPS = None;
    }
    pick
}
