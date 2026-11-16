// SPDX-License-Identifier: GPL-2.0-or-later
//! The loader under a parent bootloader (GlassineOS/boot spec 012, `docs/handback.md`):
//! headless (unless the record asks for the firmware-refreshed display of spec 003 and
//! the firmware has one), its record on the boot partition of `DeviceHandle` (FAT or
//! raw), its answer in the UEFI exit data. Everything is opened with `GetProtocol`, never exclusively, so no driver
//! of the firmware is disconnected for the parent's later use.
use alloc::alloc::{Layout, alloc, dealloc};
use alloc::string::String;
use alloc::vec::Vec;
use glassine_loader_core::answer::{Answer, Boot};
use glassine_loader_core::menu;
use glassine_loader_core::record::InUse;
use glassine_loader_core::plan::{self, Log, Platform, ReadErr, RecordStore};
use glassine_loader_core::raw::{Blocks, Raw};
use uefi::boot::{self, MemoryType, OpenProtocolAttributes, OpenProtocolParams, ScopedProtocol};
use uefi::proto::loaded_image::LoadedImage;
use uefi::proto::media::block::BlockIO;
use uefi::proto::media::fs::SimpleFileSystem;
use uefi::{Handle, Status};

use crate::display::epdfw::EpdFw;
use crate::log;

/// What the parent asked for in the load options.
pub struct Opts {
    pub serial: bool,
    pub pick_recovery: bool,
}

fn get<P: uefi::proto::ProtocolPointer + ?Sized>(h: Handle) -> uefi::Result<ScopedProtocol<P>> {
    let params = OpenProtocolParams { handle: h, agent: boot::image_handle(), controller: None };
    unsafe { boot::open_protocol::<P>(params, OpenProtocolAttributes::GetProtocol) }
}

/// The load options' words, if they put the loader in hand-back mode.
pub fn options() -> Option<Opts> {
    let li = get::<LoadedImage>(boot::image_handle()).ok()?;
    let text: String = alloc::format!("{}", li.load_options_as_cstr16().ok()?);
    let words: Vec<&str> = text.split(' ').collect();
    if !words.contains(&"glassine.handback=1") {
        return None;
    }
    Some(Opts { serial: words.contains(&"glassine.log=serial"), pick_recovery: words.contains(&"glassine.pick=recovery") })
}

/// The partition's block I/O. Reads and writes go through a buffer aligned as the
/// medium asks (`IoAlign`).
struct FwBlocks {
    io: ScopedProtocol<BlockIO>,
}

impl FwBlocks {
    fn layout(&self, len: usize) -> Option<Layout> {
        let a = (self.io.media().io_align() as usize).max(64);
        if !a.is_power_of_two() || len == 0 {
            return None;
        }
        Layout::from_size_align(len, a).ok()
    }
}

impl Blocks for FwBlocks {
    fn block_size(&self) -> u32 {
        self.io.media().block_size()
    }
    fn blocks(&self) -> u64 {
        self.io.media().last_block().saturating_add(1)
    }
    fn read_only(&self) -> bool {
        self.io.media().is_read_only()
    }
    fn read(&mut self, lba: u64, buf: &mut [u8]) -> bool {
        let Some(l) = self.layout(buf.len()) else { return false };
        let p = unsafe { alloc(l) };
        if p.is_null() {
            return false;
        }
        let b = unsafe { core::slice::from_raw_parts_mut(p, buf.len()) };
        let id = self.io.media().media_id();
        let ok = self.io.read_blocks(id, lba, b).is_ok();
        if ok {
            buf.copy_from_slice(b);
        }
        unsafe { dealloc(p, l) };
        ok
    }
    fn write(&mut self, lba: u64, buf: &[u8]) -> bool {
        let Some(l) = self.layout(buf.len()) else { return false };
        let p = unsafe { alloc(l) };
        if p.is_null() {
            return false;
        }
        let b = unsafe { core::slice::from_raw_parts_mut(p, buf.len()) };
        b.copy_from_slice(buf);
        let id = self.io.media().media_id();
        let r = self.io.write_blocks(id, lba, b);
        unsafe { dealloc(p, l) };
        if let Err(e) = &r {
            crate::log!("write at block {}: {:?}", lba, e.status());
        }
        r.is_ok()
    }
    fn now_ms(&mut self) -> u64 {
        crate::arch::now_ms()
    }
    fn flush(&mut self) -> bool {
        match self.io.flush_blocks() {
            Ok(()) => true,
            Err(e) => {
                crate::log!("flush: {:?}", e.status());
                false
            }
        }
    }
}

struct Disk {
    raw: Raw<FwBlocks>,
}

impl RecordStore for Disk {
    fn read_slot(&mut self, slot: usize) -> Result<Vec<u8>, ReadErr> {
        self.raw.read_slot(slot)
    }
    fn write_slot(&mut self, slot: usize, bytes: &[u8]) -> bool {
        self.raw.write_slot(slot, bytes)
    }
}

impl glassine_loader_core::plan::SplashStore for Disk {
    fn read_image(&mut self, which: plan::Image, max: u64, deadline_ms: Option<u64>) -> Result<Vec<u8>, ReadErr> {
        self.raw.read_image(which, max, deadline_ms)
    }
    fn has_override(&mut self) -> bool {
        self.raw.has_override()
    }
    fn remove_override(&mut self) -> bool {
        self.raw.remove_override()
    }
    fn now_ms(&mut self) -> u64 {
        crate::arch::now_ms()
    }
}

impl Log for Disk {
    fn log(&mut self, line: core::fmt::Arguments) {
        log::line(line);
    }
}

/// The boot partition in the form the loader found it in (boot `docs/boot-partition.md`,
/// "Two forms"): FAT when the firmware offers a file system on it, raw otherwise.
enum Store {
    Fat(crate::fs::Volume),
    Raw(Disk),
}

/// Opens the boot partition: the partition of our `DeviceHandle`.
fn open() -> Result<(Store, Handle), &'static str> {
    let dev = get::<LoadedImage>(boot::image_handle()).ok().and_then(|li| li.device()).ok_or("no DeviceHandle")?;
    if let Ok(mut fs) = get::<SimpleFileSystem>(dev) {
        if let Ok(root) = fs.open_volume() {
            log!("HANDBACK form=fat");
            return Ok((Store::Fat(crate::fs::Volume { root }), dev));
        }
    }
    let io = get::<BlockIO>(dev).map_err(|_| "no block I/O on DeviceHandle")?;
    if !io.media().is_media_present() {
        return Err("no medium");
    }
    let raw = Raw::new(FwBlocks { io }).map_err(|e| {
        crate::log!("the partition can't be used: {:?}", e);
        "unusable partition"
    })?;
    match &raw.regions {
        Some((copy, regs)) => log!("HANDBACK form=raw table={:?} regions={}", copy, regs.len()),
        None => log!("HANDBACK form=raw no table: the partition holds nothing"),
    }
    Ok((Store::Raw(Disk { raw }), dev))
}

/// The probe report's place in the FAT form (`docs/handback.md`, "The probe report").
const PROBE_FILE: &str = "\\glassine\\probe.txt";

/// Decides, writes what the decision writes, and returns to the parent. Never
/// returns. Headless, the splash store is left as it is: such a start draws nothing, so
/// it uses up no override (boot spec 012 FR-015a); with `display=epd-fw` it is used as
/// on a PC (spec 003).
pub fn run(opts: Opts) -> ! {
    log!("HANDBACK start serial={} pick_recovery={}", opts.serial as u8, opts.pick_recovery as u8);
    let answer = match open() {
        Err(why) => {
            log!("HANDBACK {}: the baseline", why);
            Answer::new(Boot::Baseline, "no-record")
        }
        Ok((Store::Raw(mut d), dev)) => {
            let mut rec = plan::read_record(&mut d);
            let touch = touch_probe(&mut d, &mut rec);
            let probe = touch.is_some() || rec.as_ref().is_some_and(|r| r.rec.get("probe") == "1");
            let write = |d: &mut Disk| {
                let report = crate::probe::report(Some(dev), touch.as_deref().unwrap_or(&[]));
                let ok = d.raw.write_probe(&report);
                log!("PROBE_WRITTEN ok={} bytes={} form=raw", ok as u8, report.len());
            };
            if probe {
                write(&mut d);
            }
            let (a, menu) = decide_then_draw(&mut d, rec, opts.pick_recovery);
            // Again after the menu (or MENU_NO_INPUT), so its log holds those lines (004
            // FR-014).
            if probe && menu {
                write(&mut d);
            }
            a
        }
        Ok((Store::Fat(mut v), dev)) => {
            let mut rec = plan::read_record(&mut v);
            let touch = touch_probe(&mut v, &mut rec);
            let probe = touch.is_some() || rec.as_ref().is_some_and(|r| r.rec.get("probe") == "1");
            let write = |v: &mut crate::fs::Volume| {
                let report = crate::probe::report(Some(dev), touch.as_deref().unwrap_or(&[]));
                let ok = Platform::write_file(v, PROBE_FILE, &report);
                log!("PROBE_WRITTEN ok={} bytes={} form=fat", ok as u8, report.len());
            };
            if probe {
                write(&mut v);
            }
            let (a, menu) = decide_then_draw(&mut v, rec, opts.pick_recovery);
            if probe && menu {
                write(&mut v);
            }
            a
        }
    };
    let data = answer.exit_data();
    log!("HANDBACK_ANSWER {}", String::from_utf8_lossy(&answer.text()).replace('\n', " "));
    give_back(&data)
}

/// `touch=probe` (spec 005 FR-004): the key cleared in its own write first, so a
/// bring-up that hangs inside the firmware doesn't repeat at the next start; then one
/// bring-up of the touch controller. Its report lines, or `None` without the key. When
/// the write fails the controller isn't touched.
fn touch_probe<S: RecordStore + Log>(store: &mut S, rec: &mut Option<InUse>) -> Option<Vec<String>> {
    let cur = rec.as_mut().filter(|r| r.rec.get("touch") == "probe")?;
    if !plan::write_changes(store, cur, &[("touch", "")]) {
        log!("TOUCH_SKIPPED not-written");
        return Some(alloc::vec![String::from("touch.result=skipped not-written")]);
    }
    Some(crate::touch::probe())
}

/// Whether the record asks for the firmware-refreshed display (`display=epd-fw`).
fn wants_epd(rec: Option<&InUse>) -> bool {
    rec.is_some_and(|r| r.rec.get("display") == "epd-fw")
}

fn pairs<'a>(v: &'a [(&'static str, String)]) -> Vec<(&'static str, &'a str)> {
    v.iter().map(|(k, x)| (*k, x.as_str())).collect()
}

/// The decision and its writes, then, with `display=epd-fw`, the draw (spec 003 FR-009,
/// FR-010) or the start menu (spec 004): the count is durable before any refresh runs,
/// `drew` is cleared after the last one returned; a start after an unmarked one that
/// drew and never came back sets the sticky `display_failed`, which only the running
/// system clears. Returns the answer and whether the menu was shown or would have been
/// but for a tap source.
fn decide_then_draw<S: RecordStore + plan::SplashStore + Log>(store: &mut S, rec: Option<InUse>, pick_recovery: bool) -> (Answer, bool) {
    // A `request=menu` that opens no menu is cleared in the start's first write (004
    // FR-002).
    let clear_menu: Vec<(&'static str, String)> =
        if rec.as_ref().is_some_and(|r| r.rec.get("request") == "menu") { alloc::vec![("request", String::new())] } else { Vec::new() };
    crate::touch::set_on(rec.as_ref().is_some_and(|r| r.rec.get("touch") == "on"));
    if !wants_epd(rec.as_ref()) {
        return (plan::handback_with(store, rec, pick_recovery, &pairs(&clear_menu), &[]), false);
    }
    let ep = plan::epd_plan(rec.as_ref());
    let mut found: Option<Result<EpdFw, String>> = None;
    // Whether the menu would have opened but for a tap source (004 FR-014).
    let mut no_input = false;
    if let (Some(why), Some(entry)) = (menu::opens(rec.as_ref(), &ep, pick_recovery), rec.as_ref()) {
        found = Some(crate::display::epdfw::find());
        match found.as_mut().unwrap() {
            Ok(epd) => {
                let (w, h) = epd.size_native();
                match crate::epdmenu::tap_source(w, h) {
                    Some(src) => {
                        if let Some(a) = start_menu(store, epd, entry, &ep, why, src) {
                            return (a, true);
                        }
                    }
                    // No tap source: no menu, the default at once, counted as without
                    // one (spec 004 FR-001).
                    None => {
                        log!("MENU_NO_INPUT why={}: no tap source, the default", why.name());
                        no_input = true;
                    }
                }
            }
            Err(e) => log!("EPD_FW_NONE {}: no menu", e),
        }
    }
    let mut any = ep.any.clone();
    any.extend(clear_menu);
    let answer = plan::handback_with(store, rec, pick_recovery, &pairs(&any), &pairs(&ep.counted));
    match plan::epd_draws(&ep, &answer) {
        Ok(()) => {
            match found.unwrap_or_else(crate::display::epdfw::find) {
                Ok(mut epd) => epd_splash(store, &mut epd),
                Err(why) => log!("EPD_FW_NONE {}: headless", why),
            }
            // The draw returned (or there was none): `drew` cleared, so drew=1 at the
            // next start means only a draw that never came back (004 FR-007).
            match plan::read_record(store) {
                Some(mut cur) => {
                    if !plan::write_changes(store, &mut cur, &[("drew", "")]) {
                        log!("DREW_NOT_CLEARED: the record can't be written");
                    }
                }
                None => log!("DREW_NOT_CLEARED: no record"),
            }
        }
        Err(why) => log!("EPD_FW_SKIPPED {}", why),
    }
    (answer, no_input)
}

/// The start menu (spec 004): the menu write, the menu, the splash, the pick's write.
/// `None`: the menu write failed and no menu opened.
fn start_menu<S: RecordStore + plan::SplashStore + Log>(
    store: &mut S,
    epd: &mut EpdFw,
    entry: &InUse,
    ep: &plan::EpdPlan,
    why: menu::Why,
    src: alloc::boxed::Box<dyn crate::epdmenu::TapSource>,
) -> Option<Answer> {
    epd.partial = entry.rec.get("display_partial") != "0";
    let mut cur = entry.clone();
    let mut w1 = ep.any.clone();
    w1.extend(menu::open_write(entry));
    if !plan::write_changes(store, &mut cur, &pairs(&w1)) {
        log!("MENU_NOT_OPENED the menu write failed");
        return None;
    }
    log!("MENU_OPEN why={} partial={} {}", why.name(), epd.partial as u8, epd.describe());
    let choice = crate::epdmenu::run(epd, entry, src);
    log!("MENU_CLOSED {:?}", choice);
    epd_splash(store, epd);
    let d = menu::decide(entry, &choice);
    let answer = if plan::write_changes(store, &mut cur, &pairs(&menu::pick_write(entry, &d))) {
        d.answer
    } else {
        log!("the pick's write failed: the answer as for a record that can't be written");
        menu::not_written(&choice)
    };
    crate::watchdog::arm();
    Some(answer)
}

/// The splash through the firmware's panel refresh (boot `docs/handback.md`, "A display
/// through the firmware's refresh"): the override used up and the picture picked as on
/// a PC, composed in portrait, written and refreshed once, after the record's write and
/// before the answer. No key line: which key a person would press at power-on is not
/// defined on such a machine.
fn epd_splash<S: plan::SplashStore + Log>(store: &mut S, epd: &mut EpdFw) {
    log!("EPD_FW {}", epd.describe());
    let (w, h) = epd.size();
    let (pic, what) = match plan::pick_picture(store, &[(w, h)]) {
        plan::Pick::Override(p) => (p, "override"),
        plan::Pick::Splash(p, n) => (p, n),
        plan::Pick::LeaveScreen => {
            log!("SPLASH_DRAWN leave-screen");
            return;
        }
        plan::Pick::Nothing => {
            log!("SPLASH_DRAWN none");
            return;
        }
    };
    let mut buf = alloc::vec![0u32; (w as usize) * (h as usize)];
    glassine_loader_core::gspl::compose(&pic, w, h, &mut buf);
    drop(pic);
    let t0 = crate::arch::now_ms();
    let st = epd.show(&buf);
    log!("SPLASH_DRAWN {} refresh={:?} in {} ms", what, st, crate::arch::now_ms() - t0);
}

/// `Exit` with `data` as the exit data, in a buffer from `AllocatePool` that the parent
/// frees. UEFI keeps exit data only with a status other than `EFI_SUCCESS`.
fn give_back(data: &[u8]) -> ! {
    let (ptr, size) = match boot::allocate_pool(MemoryType::LOADER_DATA, data.len()) {
        Ok(p) => {
            unsafe { core::ptr::copy_nonoverlapping(data.as_ptr(), p.as_ptr(), data.len()) };
            (p.as_ptr() as *mut uefi::Char16, data.len())
        }
        Err(_) => {
            log!("no pool for the answer: none given");
            (core::ptr::null_mut(), 0)
        }
    };
    unsafe {
        let _ = boot::exit(boot::image_handle(), Status::ABORTED, size, ptr);
    }
    crate::arch::halt()
}
